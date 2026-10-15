use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, seed_bound_work_item, tempdir},
};
use crate::runtime_db::{
    input_evidence_snapshot::InputEvidenceSnapshotError,
    task_snapshot::TaskDomainLoadRequest,
    transitions::{
        AgentStateMutation, InputMutationExecution, PreparedInputTransition,
        PreparedQueueTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
    },
    work_snapshot::WorkDomainLoadRequest,
};
use crate::types::WorkReactivationMode;
async fn fixture(current: bool) -> Result<(tempfile::TempDir, RuntimeHandle, String)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "default",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let id = seed_bound_work_item(&runtime, WorkItemState::Open, None, None).await;
    let mut owner = runtime.begin_mutation().await?;
    owner.state.status = if current {
        AgentStatus::AwakeRunning
    } else {
        AgentStatus::Asleep
    };
    owner.state.current_run_id = current.then(|| "actual-sleep-active-run".into());
    owner.state.current_work_item_id = current.then(|| id.clone());
    owner.persist_state(&runtime.inner.storage).await?;
    drop(owner);
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime, id))
}
async fn observation(runtime: &RuntimeHandle) -> Result<OriginalSleepOverrideObservation> {
    let read = runtime.capture_scheduler_projection().await?;
    let (work, mode, generation) = read
        .projection
        .work_reactivation_work_item_with_generation()
        .context("actual Work must be runnable")?;
    runtime.prepare_original_sleep_override_observation(
        &read,
        work,
        generation,
        match mode {
            WorkReactivationMode::ContinueActive => "continue_active",
            WorkReactivationMode::ActivateQueued => "queued_available",
        },
    )
}
fn decision(observation: &OriginalSleepOverrideObservation) -> scheduler::SchedulerDecision {
    scheduler::SchedulerDecision::new(
        scheduler::SchedulerDecisionKind::EmitSystemTick,
        observation.reason,
    )
    .boundary(scheduler::SchedulerBoundary::LifecycleSleep.as_str())
    .model_reentry(true)
    .work_item_id(observation.work_item.id.clone())
    .evidence("lifecycle_sleep_overridden_by_runnable_work")
}
fn events(runtime: &RuntimeHandle) -> Result<Vec<AuditEvent>> {
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    Ok(root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
        .events
        .iter()
        .filter_map(|r| match &r.fact {
            CurrentReadFact::Event(e) => Some(e.clone()),
            _ => None,
        })
        .collect())
}
fn commit(
    runtime: &RuntimeHandle,
    p: &PreparedInputTransition,
) -> Result<InputMutationExecution<TransitionCommit>> {
    runtime
        .runtime_db()
        .transitions_in_scope(&p.descriptor().scopes()[0])
        .commit_prepared_transition_with_work_request_and_memory_budget(
            p,
            &WorkDomainLoadRequest {
                capture: resident_work::WorkCaptureRequest::current(),
                byte_limit: 64 * 1024 * 1024,
                record_limit: 65536,
            },
            &runtime.inner.input_evidence_budget,
            &TaskDomainLoadRequest {
                capture: resident_task::TaskCaptureRequest::current(),
                query: Default::default(),
                byte_limit: 64 * 1024 * 1024,
                record_limit: 65536,
            },
        )
}
#[tokio::test]
async fn actual_sleep_override_retains_pre_tick_projection_and_separate_tick_then_whole_pair(
) -> Result<()> {
    for current in [false, true] {
        let (_home, runtime, id) = fixture(current).await?;
        let before = runtime.agent_state().await?;
        let receipts: usize = runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts",
            [],
            |r| r.get(0),
        )?;
        runtime.transition_to_sleep(None).await?;
        let after = runtime.agent_state().await?;
        assert_eq!(after.pending, before.pending + 1);
        assert_eq!(after.current_run_id, before.current_run_id);
        assert!(after.sleeping_until.is_none());
        let all = events(&runtime)?;
        let pair = all
            .iter()
            .filter(|e| {
                e.kind == "scheduler_posture_decision"
                    && e.data["boundary"] == "lifecycle_sleep"
                    && e.data["reason"] == "sleep_overridden_runnable_work"
            })
            .collect::<Vec<_>>();
        assert_eq!(pair.len(), 1);
        let changed = all
            .iter()
            .filter(|e| e.kind == "agent_state_changed")
            .max_by_key(|e| e.event_seq)
            .unwrap();
        assert_eq!(
            changed.data,
            serde_json::to_value(AgentStateChangedEvent::from_state(&before))?,
            "observation must not be replaced by the later Tick State"
        );
        assert_eq!(
            pair[0].data["previous_status"],
            serde_json::to_value(&before.status)?
        );
        assert_eq!(
            pair[0].data["next_status"],
            serde_json::to_value(&before.status)?
        );
        assert!(pair[0].data["evidence"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(format!("work_item_id={id}"))));
        let tick = all
            .iter()
            .filter(|e| e.kind == "system_tick_emitted" && e.data["subsystem"] == "work_queue")
            .max_by_key(|e| e.event_seq)
            .unwrap();
        assert!(tick.event_seq < pair[0].event_seq && pair[0].event_seq < changed.event_seq);
        assert_eq!(
            runtime.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts",
                [],
                |r| r.get::<_, usize>(0)
            )?,
            receipts + 2,
            "original Tick plus one observation whole receipt"
        );
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let root_state = root.state.clone();
        let _pause = runtime.pause_resource_observer_for_test().await;
        runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO forbidden_sleep_events;ALTER TABLE agent_states RENAME TO forbidden_sleep_states;")?;
        assert!(root
            .current_reads
            .as_ref()
            .unwrap()
            .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
            .events
            .iter()
            .any(|r| matches!(&r.fact,CurrentReadFact::Event(e) if e.id==changed.id)));
        assert_eq!(root.state, root_state);
    }
    Ok(())
}
#[tokio::test]
async fn accepted_sleep_override_completes_actual_tick_and_pair_after_caller_abort_and_closing(
) -> Result<()> {
    let (_home, runtime, _) = fixture(true).await?;
    let observation = observation(&runtime).await?;
    let command = decision(&observation);
    let work_weak = Arc::downgrade(&observation.material.work.as_ref().unwrap().0);
    let light_weak = Arc::downgrade(&observation.material.light);
    // This actual observer has been quiesced in fixture. Wait for previous
    // accepted helpers to finish, then only the new override can make this
    // original registry non-finished while its owner is held.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !runtime.inner.mutation_tasks.lock().unwrap().all_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let owner = runtime.begin_mutation().await?;
    let before = owner.state.clone();
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .start_owned_sleep_override(observation, command)
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while runtime.inner.mutation_tasks.lock().unwrap().all_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    runtime.close_mutation_admission();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(work_weak.upgrade().is_some());
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_owned_mutations(),
    )
    .await??;
    assert_eq!(runtime.agent_state().await?.pending, before.pending + 1);
    assert_eq!(
        events(&runtime)?
            .iter()
            .filter(|e| e.kind == "scheduler_posture_decision"
                && e.data["reason"] == "sleep_overridden_runnable_work")
            .count(),
        1
    );
    assert!(work_weak.upgrade().is_none() && light_weak.upgrade().is_none());
    Ok(())
}
#[tokio::test]
async fn original_sleep_pair_rolls_back_or_reconciles_without_repeating_the_actual_tick(
) -> Result<()> {
    for mode in ["rollback", "known", "unknown"] {
        let (_home, runtime, _) = fixture(true).await?;
        let observation = observation(&runtime).await?;
        let command = decision(&observation);
        let work_weak = Arc::downgrade(&observation.material.work.as_ref().unwrap().0);
        let light_weak = Arc::downgrade(&observation.material.light);
        runtime
            .emit_system_tick_from_original_sleep_override(&observation, Some(&command))
            .await?;
        let (frozen_events, light) = runtime.freeze_original_sleep_override_audits(observation)?;
        assert!(
            work_weak.upgrade().is_none(),
            "dead Work copy must not occupy observation allowance"
        );
        let ids = [frozen_events[0].id.clone(), frozen_events[1].id.clone()];
        let times = [frozen_events[0].created_at, frozen_events[1].created_at];
        let mut owner = runtime.begin_mutation().await?;
        let root = owner.capture_published_root()?;
        let state = root.state.clone();
        let revision = root.current_reads.as_ref().unwrap().coverage().revision;
        let p = PreparedInputTransition::runtime_sleep_override_audits(
            owner.input_evidence_scope(),
            frozen_events,
            state.clone(),
            light,
            runtime.instance_owner_epoch(),
            &runtime.inner.input_evidence_budget,
            match mode {
                "rollback" => Some(TransitionFaultPoint::AfterAuditWrites),
                "known" => Some(TransitionFaultPoint::BeforeCacheUpdate),
                _ => None,
            },
        )?;
        drop(root);
        if mode == "rollback" {
            assert!(commit(&runtime, &p).is_err());
            assert!(!events(&runtime)?.iter().any(|e| ids.contains(&e.id)));
            assert_eq!(owner.state.pending, state.pending);
            assert_eq!(
                runtime
                    .inner
                    .mutation_owner
                    .capture()?
                    .unwrap()
                    .current_reads
                    .as_ref()
                    .unwrap()
                    .coverage()
                    .revision,
                revision
            );
            drop((owner, p));
            assert!(light_weak.upgrade().is_none());
            continue;
        }
        if mode == "known" {
            assert!(owner.commit_prepared_input_transition(p).await.is_err());
        } else {
            assert!(owner
                .lose_original_sleep_pair_ack_for_test(&p, || commit(&runtime, &p))
                .await
                .is_err());
            drop(p);
        }
        drop(owner);
        assert!(matches!(
            runtime.inner.mutation_owner.capture(),
            Err(mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ));
        drop(runtime.begin_mutation().await?);
        let all = events(&runtime)?;
        for (i, id) in ids.iter().enumerate() {
            let actual = all.iter().filter(|e| &e.id == id).collect::<Vec<_>>();
            assert_eq!(actual.len(), 1);
            assert_eq!(actual[0].created_at, times[i]);
        }
        assert_eq!(
            all.iter()
                .filter(|e| e.kind == "system_tick_emitted" && e.data["subsystem"] == "work_queue")
                .count(),
            1
        );
        assert_eq!(runtime.agent_state().await?.pending, state.pending);
        assert_eq!(
            runtime
                .inner
                .mutation_owner
                .capture()?
                .unwrap()
                .current_reads
                .as_ref()
                .unwrap()
                .coverage()
                .revision,
            revision + 1
        );
        assert!(light_weak.upgrade().is_none());
    }
    Ok(())
}
#[tokio::test]
async fn sleep_tick_material_is_debited_in_original_prefix_before_state_decoder() -> Result<()> {
    let (_home, runtime, id) = fixture(true).await?;
    let observation = observation(&runtime).await?;
    let loan = observation.material.clone();
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let scope = loan.scope.clone();
    let ledger = &runtime.inner.input_evidence_budget;
    let source = reservation.input_query_source(&scope, ledger, (64 * 1024 * 1024, 65536))?;
    let mut message = MessageEnvelope::new(
        scope.agent_id.clone(),
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original exact Tick prefix".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    message.work_item_id = Some(id);
    message.normalize_admission_fields();
    message.turn_id = Some(crate::ids::turn_id());
    let mut after = root.state.as_ref().clone();
    after.pending += 1;
    let queue = QueueEntryRecord {
        message_id: message.id.clone(),
        agent_id: scope.agent_id.clone(),
        priority: message.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: message.created_at,
        updated_at: runtime.now(),
    };
    let p = PreparedInputTransition::queue(
        scope.clone(),
        "sleep-prefix-before-state".into(),
        runtime.now(),
        PreparedQueueTransition::new(QueueTransitionCommand {
            agent_id: scope.agent_id.clone(),
            operation: QueueOperation::Admit,
            mutation: QueueMutation::Upsert(queue),
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(root.state.as_ref().clone())),
                record: Box::new(after),
            }),
            message_evidence: vec![message],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        }),
    )?
    .with_current_read_source(root.current_reads.as_ref().unwrap().clone())?
    .with_original_sleep_override_material(loan.clone(), reservation.owner_epoch(), ledger)?;
    let prior = loan.material_estimate()?;
    let current = root.current_reads.as_ref().unwrap().material_estimate()?;
    let bytes = prior
        .0
        .checked_add(current.0)
        .context("Sleep prefix boundary overflow")?
        .checked_sub(1)
        .unwrap();
    let raw: String = runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [runtime.instance_agent_id()],
        |r| r.get(0),
    )?;
    runtime.runtime_db().connection()?.execute(
        "UPDATE agent_states SET payload_json='not-json' WHERE agent_id=?1",
        [runtime.instance_agent_id()],
    )?;
    let work = WorkDomainLoadRequest {
        capture: resident_work::WorkCaptureRequest::current(),
        byte_limit: bytes,
        record_limit: 65536,
    };
    let tasks = TaskDomainLoadRequest {
        capture: resident_task::TaskCaptureRequest::current(),
        query: Default::default(),
        byte_limit: bytes,
        record_limit: 65536,
    };
    let error = runtime
        .runtime_db()
        .transitions_in_scope(&scope)
        .commit_prepared_transition_with_input_query_source(&p, &work, ledger, &tasks, &source)
        .err()
        .context("Sleep material was not debited before State")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='sleep-prefix-before-state'",[],|r|r.get::<_,usize>(0))?,0);
    runtime.runtime_db().connection()?.execute(
        "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
        rusqlite::params![raw, runtime.instance_agent_id()],
    )?;
    Ok(())
}
