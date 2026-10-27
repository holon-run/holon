use super::*;
use crate::runtime::{
    tests::support::{context_config, tempdir, StubProvider},
    WaitForWakeKind,
};

fn fixture() -> (tempfile::TempDir, RuntimeHandle) {
    let home = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "scheduler-resident",
        home.path().into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "scheduler-resident".into(),
        context_config(),
    )
    .unwrap();
    (home, runtime)
}

#[tokio::test]
async fn scheduler_error_metadata_preserves_window_agent_and_timestamp_order() -> Result<()> {
    let (_home, runtime) = fixture();
    let db = runtime.runtime_db();
    let at = chrono::DateTime::from_timestamp_millis(1_760_000_000_123).unwrap();
    for i in 0..70 {
        let mut event = AuditEvent::legacy(
            if matches!(i, 0 | 6 | 68) {
                "runtime_error"
            } else {
                "scheduler_metadata"
            },
            serde_json::json!({}),
        );
        // The newest timestamp is outside the window; sequence order and
        // error timestamp order intentionally disagree within it.
        event.created_at = at + chrono::Duration::seconds(100 - i);
        db.audit_events()
            .append(Some("scheduler-resident"), &event)?;
    }
    let connection = db.connection()?;
    assert_eq!(
        scheduler_latest_runtime_error_on(&connection, "scheduler-resident")?,
        Some(at + chrono::Duration::seconds(94))
    );
    drop(connection);
    db.audit_events().append(
        Some("another-agent"),
        &AuditEvent::legacy("runtime_error", serde_json::json!({})),
    )?;
    db.audit_events().append(
        Some("scheduler-resident"),
        &AuditEvent::legacy("scheduler_metadata", serde_json::json!({})),
    )?;
    let connection = db.connection()?;
    assert_eq!(
        scheduler_latest_runtime_error_on(&connection, "scheduler-resident")?,
        Some(at + chrono::Duration::seconds(32))
    );
    assert_eq!(
        scheduler_latest_runtime_error_on(&connection, "absent-agent")?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn scheduler_error_metadata_never_reads_or_materializes_event_bodies() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let (_home, runtime) = fixture();
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: "scheduler-resident".into(),
        identity_incarnation: 1,
    };
    let mut event = AuditEvent::legacy(
        "runtime_error",
        serde_json::json!({"unused": vec![0; 9000]}),
    );
    event.created_at = chrono::DateTime::from_timestamp_millis(1_760_000_000_123).unwrap();
    let event = runtime
        .runtime_db()
        .audit_events()
        .append(Some(&scope.agent_id), &event)?;
    let connection = runtime.runtime_db().connection()?;
    let body_phase = ResourceReadMaterial::new(&runtime.inner.input_evidence_budget, &scope)?;
    let error = body_phase
        .read(|| {
            let mut query =
                connection.prepare("SELECT data_json FROM audit_events WHERE agent_id=?1")?;
            crate::runtime_db::repositories::audit_event_records(&mut query, [&scope.agent_id])
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("ResourceExhausted"));
    assert!(body_phase.finish().is_err());
    connection.execute_batch(
        "ALTER TABLE audit_events RENAME TO scheduler_event_bodies_forbidden;
         CREATE VIEW audit_events AS
         SELECT agent_id,kind,created_at,event_seq FROM scheduler_event_bodies_forbidden;",
    )?;
    let phase = ResourceReadMaterial::new(&runtime.inner.input_evidence_budget, &scope)?;
    let baseline = phase.finish()?;
    assert_eq!(
        phase.read(|| scheduler_latest_runtime_error_on(&connection, &scope.agent_id))?,
        Some(event.created_at)
    );
    assert_eq!(phase.finish()?, baseline);
    assert!(body_phase.finish().is_err());
    // Type errors in metadata are still errors, even for a non-error event.
    connection.execute(
        "UPDATE scheduler_event_bodies_forbidden SET kind='other',created_at=x'00'",
        [],
    )?;
    assert!(scheduler_latest_runtime_error_on(&connection, &scope.agent_id).is_err());
    Ok(())
}

#[tokio::test]
async fn ordinary_work_history_keeps_array_wire_and_last_acquisition_holder() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let (_home, runtime) = fixture();
    let record = WorkItemRecord::new(
        "scheduler-resident",
        "owned history {,:} 中文".repeat(32),
        WorkItemState::Open,
    );
    assert!(runtime.inner.runtime_db.work_items().insert_new(&record)?);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let all = runtime.latest_work_items().await?;
    let selected = runtime
        .latest_work_items_for_agent("scheduler-resident", 1)
        .await?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    for read in [&all, &selected] {
        assert_eq!(read.len(), 1);
        assert_eq!(read[0], record);
        assert_eq!(serde_json::to_value(read)?, serde_json::json!([record]));
        assert!(!format!("{read:?}").contains("BudgetViewGuard"));
    }
    drop(selected);
    let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let first = Arc::new(all);
    let last = first.clone();
    drop(first);
    assert_eq!(
        serde_json::to_value(last.as_ref())?,
        serde_json::json!([record])
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    drop(last);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let scope = runtime.begin_mutation().await?.input_evidence_scope();
    let phase = ResourceReadMaterial::with_test_limits(
        &runtime.inner.input_evidence_budget,
        &scope,
        (scope.agent_id.len() as u64 + 256) * 8,
        64,
    )?;
    {
        let _entry = phase.enter();
        assert!(runtime.latest_work_items().await.is_err());
        assert!(phase.finish().is_err());
        assert!(runtime
            .latest_work_items_for_agent("absent", 0)
            .await
            .is_err());
    }
    drop(phase);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn idle_scheduler_prepares_its_window_and_replans_new_queue_input() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::InputWindowRequest;
    let (_home, runtime) = fixture();
    drop(runtime.prepare_idle_scheduler_projection().await?.unwrap());

    let mut mutation = runtime.begin_mutation().await?;
    let scope = mutation.input_evidence_scope();
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let request = InputEvidenceLoadRequest {
        query: None,
        windows: vec![InputWindowRequest {
            selector: InputWindowSelector::RecentBriefs,
            limit: 1,
        }],
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit,
        byte_limit,
    };
    // Model the narrower window installed by an intervening original producer.
    let prepared = runtime.inner.runtime_db.transaction(|tx| {
        crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
            tx,
            &scope,
            &mutation.state,
            &request,
            context_config().recent_episode_candidates,
            &runtime.inner.input_evidence_budget,
            &crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
        )
    })?;
    mutation.input_evidence = Some(Arc::new(
        prepared
            .inputs
            .with_budget(runtime.inner.input_evidence_budget.clone())?,
    ));
    mutation.install_transient().await?;
    drop(mutation);

    let error = runtime
        .capture_scheduler_projection()
        .await
        .err()
        .context("ordinary scheduler capture must reject the narrow window")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime::resident_input_evidence::InputEvidenceReadError>(),
        Some(crate::runtime::resident_input_evidence::InputEvidenceReadError::WindowNotPrepared)
    ));
    drop(runtime.prepare_idle_scheduler_projection().await?.unwrap());
    let mutation = runtime.begin_mutation().await?;
    assert!(mutation
        .capture_input_evidence()?
        .window(
            &InputWindowSelector::RecentBriefs,
            crate::runtime::scheduler::SCHEDULER_BRIEF_LIMIT,
        )?
        .is_empty());
    drop(mutation);
    drop(runtime.capture_scheduler_projection().await?);

    runtime
        .enqueue(MessageEnvelope::new(
            "scheduler-resident",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "arrived after the idle poll".into(),
            },
        ))
        .await?;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(runtime.prepare_idle_scheduler_projection().await?.is_none());
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(before.token, after.token);
    assert_eq!(after.state.pending, 1);
    Ok(())
}

#[tokio::test]
async fn scheduler_acquisition_enters_without_tls_and_keeps_its_last_holder() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let (_home, runtime) = fixture();
    let mut mutation = runtime.begin_mutation().await?;
    let now = runtime.now();
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), now)
        .await?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = read.acquisition.guard()?;
    let weak = Arc::downgrade(&last);
    let expected = SchedulerProjection::from_state_with_queue_len_at(
        runtime.storage(),
        &read.state,
        mutation.queue.len(),
        now,
    )?;
    assert_eq!(read.projection, expected);
    drop(read);
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn scheduler_acquisition_reuses_and_latches_original_phase() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let (_home, runtime) = fixture();
    let mut mutation = runtime.begin_mutation().await?;
    drop(
        mutation
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?,
    );
    let scope = mutation.input_evidence_scope();
    let header = (scope.agent_id.len() as u64 + 256) * 8;
    let phase = ResourceReadMaterial::with_test_limits(
        &runtime.inner.input_evidence_budget,
        &scope,
        header,
        64,
    )?;
    {
        let _entry = phase.enter();
        assert!(mutation
            .capture_scheduler_read(&WorkCaptureRequest::current(), runtime.now())
            .is_err());
        assert!(phase.finish().is_err());
        assert!(mutation
            .capture_scheduler_read(&WorkCaptureRequest::current(), runtime.now())
            .is_err());
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[tokio::test]
async fn scheduler_capture_does_not_spend_retained_input_query_acquisition() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
    let (_home, runtime) = fixture();
    let (_state, _session) = runtime.begin_task_birth_session().await?;
    runtime
        .enqueue(MessageEnvelope::new(
            "scheduler-resident",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "retain the actual scheduler binding".into(),
            },
        ))
        .await?;
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime).poll().await?
    else {
        anyhow::bail!("source was not claimed");
    };
    runtime
        .begin_interactive_turn(Some(&scheduled.message), None, None)
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    assert!(mutation.state.current_execution_binding.is_some());
    drop(
        mutation
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?,
    );
    let source = mutation
        .input_evidence
        .as_ref()
        .context("Input domain missing")?
        .capture()?;
    let original = source
        .query_acquisition
        .as_ref()
        .context("Input query acquisition missing")?;
    let (_, used) = original.finish()?;
    let limit = usize::try_from(
        runtime
            .inner
            .input_evidence_budget
            .limits()
            .record_limit
            .min(65_536),
    )?;
    let remaining = limit
        .checked_sub(used)
        .context("query already exceeds limit")?;
    anyhow::ensure!(remaining % 8 == 0, "query record allowance is not aligned");
    original.read(|| {
        crate::diagnostics::resource_io::charge_owned_metadata(0, remaining / 8)?;
        Ok(())
    })?;
    let baseline = original.finish()?;
    assert_eq!(baseline.1, limit);
    for _ in 0..3 {
        let prepared = mutation
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?;
        assert_eq!(original.finish()?, baseline);
        let guard = prepared.acquisition.guard()?;
        let last = Arc::downgrade(&guard);
        drop(guard);
        assert!(last.upgrade().is_some());
        drop(prepared);
        assert!(last.upgrade().is_none());
        let phase = ResourceReadMaterial::new(
            &runtime.inner.input_evidence_budget,
            &mutation.input_evidence_scope(),
        )?;
        let header = phase.finish()?;
        let read = phase.read(|| {
            mutation.capture_scheduler_read(&WorkCaptureRequest::current(), runtime.now())
        })?;
        assert!(phase.finish()?.1 > header.1);
        assert_eq!(original.finish()?, baseline);
        drop(read);
    }
    assert!(original
        .read(|| {
            crate::diagnostics::resource_io::charge_owned_metadata(0, 1)?;
            Ok(())
        })
        .is_err());
    assert!(original.read(|| Ok(())).is_err());
    assert!(source.selected_material_estimate().is_err());
    assert!(mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
        .await
        .is_err());
    Ok(())
}

#[tokio::test]
async fn control_preflight_refusal_never_retains_an_unattempted_cancellation() -> Result<()> {
    let (_home, runtime) = fixture();
    let (_state, _session) = runtime.begin_task_birth_session().await?;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let (key, invocation) =
        runtime.freeze_direct_control(ControlAction::Stop, AuthorityClass::OperatorInstruction);
    let mut mutation = runtime.begin_mutation().await?;
    let prepared = runtime
        .prepare_runtime_control(&mutation, key, invocation)
        .await?;
    // Refuse a private preparation without changing the published query proof.
    mutation.input_evidence = None;
    let error = mutation.commit_control(&prepared).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::runtime::resident_input_evidence::InputQueryError>(),
        Some(crate::runtime::resident_input_evidence::InputQueryError::NeedsPreparation)
    ));
    drop(mutation);
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(before.token, after.token);
    assert_eq!(before.state, after.state);
    runtime.inner.control_cancellations.drain().await?;
    runtime.control(ControlAction::Stop).await?;
    runtime.inner.control_cancellations.drain().await?;
    Ok(())
}

#[tokio::test]
async fn loaded_borrowed_queue_keeps_rich_work_advisories_and_last_owned_holder() -> Result<()> {
    use crate::types::{TodoItem, TodoItemState, WorkItemPlanStatus};
    let (_home, runtime) = fixture();
    let waiting = runtime
        .create_work_item(
            "waiting owned queue 汉🦀".repeat(43),
            Some(WorkItemPlanStatus::Ready),
            None,
            vec![TodoItem {
                text: "retained Todo 汉🦀".repeat(31),
                state: TodoItemState::InProgress,
            }],
        )
        .await?;
    runtime
        .register_wait_for(
            "scheduler-resident",
            Some(waiting.id.clone()),
            WaitForWakeKind::External,
            Some("github:holon-run/holon#2258".into()),
            "original waiting member".into(),
            None,
        )
        .await?;
    let waiting = runtime
        .inner
        .runtime_db
        .work_items()
        .latest(&waiting.id)?
        .expect("registered wait owner remains present");
    let runnable = runtime
        .create_work_item(
            "runnable owned queue 汉🦀".repeat(47),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let now = runtime.now();
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), now)
        .await?;
    let legacy = SchedulerProjection::from_state_with_queue_len_at(
        runtime.storage(),
        &read.state,
        mutation.queue.len(),
        now,
    )?;
    assert_eq!(read.projection, legacy);
    let original_waits = runtime
        .storage()
        .active_wait_conditions_for_agent(&read.state.id)?;
    assert_eq!(read.active_waits, original_waits);
    assert_eq!(read.projection.active_waiting_intents, 1);
    assert_eq!(read.projection.active_work_item_waiting_intents, 1);
    assert_eq!(read.projection.active_agent_waiting_intents, 0);
    assert!(read.work_queue.items.iter().any(|item| {
        item.work_item.id == waiting.id
            && item.work_item.todo_list == waiting.todo_list
            && item.work_item.objective == waiting.objective
    }));
    assert!(read
        .projection
        .queued_runnable_work_items
        .iter()
        .any(|item| item == &runnable));
    assert_eq!(read.projection.waiting_work_item.as_ref(), Some(&waiting));
    assert_eq!(
        read.advisories,
        scheduler::scheduling_advisories_with_queue_len(
            runtime.storage(),
            &read.state,
            mutation.queue.len(),
        )?
    );
    assert_eq!(
        read.scheduling_posture,
        runtime.storage().agent_posture_projection(&read.state)?
    );
    let queue_value = |queue: &WorkQueueReadModel| {
        serde_json::to_value((
            &queue.current,
            &queue.queued_blocked,
            &queue.items,
            &queue.current_runnable,
            &queue.triggered_blocked,
            &queue.queued_runnable,
            &queue.yielded,
            &queue.waiting_for_operator,
            &queue.blocked,
            &queue.completing,
            &queue.completed_recent,
        ))
    };
    let original_queue = queue_value(&read.work_queue)?;
    let owned = Arc::downgrade(&read._owned_budget);
    let last_holder = Arc::clone(&read._owned_budget);
    let PreparedSchedulerRead {
        projection,
        work_queue,
        active_waits,
        _owned_budget,
        ..
    } = read;
    drop((projection, mutation, _owned_budget));
    assert!(owned.upgrade().is_some());
    assert_eq!(queue_value(&work_queue)?, original_queue);
    assert_eq!(active_waits, original_waits);
    drop((work_queue, active_waits));
    assert!(owned.upgrade().is_some());
    drop(last_holder);
    assert!(owned.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn child_observability_keeps_exact_active_tasks_and_brief_window_without_body_sql(
) -> Result<()> {
    use crate::{
        runtime_db::{
            prepared_input_evidence::PreparedInputEvidence as Write,
            transitions::TaskTransitionCommand,
        },
        types::{
            AuditEvent, BriefKind, BriefRecord, ChildAgentObservabilitySnapshot, ChildAgentPhase,
            TaskKind, TaskRecord, TaskStatus,
        },
    };
    let (_home, runtime) = fixture();
    let at = runtime.now();
    let mut mutation = runtime.begin_mutation().await?;
    mutation
        .commit_task_transition(
            "child-observability-actual-task".into(),
            TaskTransitionCommand {
                agent_id: "scheduler-resident".into(),
                expected_owner_incarnation: runtime.instance_incarnation(),
                task: TaskRecord {
                    id: "child-observability-running".into(),
                    agent_id: "scheduler-resident".into(),
                    kind: TaskKind::CommandTask,
                    status: TaskStatus::Running,
                    created_at: at,
                    updated_at: at,
                    parent_message_id: None,
                    work_item_id: None,
                    summary: Some("actual managed task".into()),
                    detail: None,
                    recovery: None,
                },
                agent_deletion: None,
                task_result_settlement: None,
                queue_entry: None,
                work_items: Vec::new(),
                expected_wait_conditions: Vec::new(),
                wait_conditions: Vec::new(),
                agent_state: None,
                message_evidence: Vec::new(),
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: false,
                commit_on_idempotent: false,
                fault: None,
            },
            Default::default(),
            true,
        )
        .await?;
    for i in 0..20 {
        let mut brief = BriefRecord::new(
            "scheduler-resident",
            if i == 0 {
                BriefKind::Ack
            } else {
                BriefKind::Result
            },
            format!("original-child-brief-{i:02}"),
            None,
            None,
        );
        brief.created_at = at + chrono::Duration::milliseconds(i);
        let prepared = Write::brief_created(
            mutation.input_evidence_scope(),
            crate::ids::audit_event_id(),
            brief.created_at,
            brief.clone(),
            AuditEvent::legacy("brief_created", serde_json::json!({"brief_id": brief.id})),
            Vec::new(),
        )?;
        mutation.commit_prepared_evidence(prepared, None).await?;
    }
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
        .await?;
    assert_eq!(read.observability_briefs.len(), 16);
    // Cold and resident reads share narrow scheduler metadata; both still
    // retain the original public Brief window and material bounds.
    let loaded_usage = read.acquisition.finish()?;
    let cold_material = crate::diagnostics::resource_io::ResourceReadMaterial::new(
        &runtime.inner.input_evidence_budget,
        &read.tasks.scope,
    )?;
    let cold_actual = RuntimeHandle::child_agent_observability_from_storage(
        runtime.storage(),
        &read.state,
        &cold_material,
    )?;
    let cold_usage = cold_material.finish()?;
    assert!(cold_usage.0 > 0 && cold_usage.1 > 0);
    for (bytes, records) in [
        cold_usage,
        (cold_usage.0 - 1, cold_usage.1),
        (cold_usage.0, cold_usage.1 - 1),
    ] {
        let bounded = crate::diagnostics::resource_io::ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &read.tasks.scope,
            bytes,
            records,
        )?;
        let actual = RuntimeHandle::child_agent_observability_from_storage(
            runtime.storage(),
            &read.state,
            &bounded,
        );
        if (bytes, records) == cold_usage {
            assert_eq!(
                serde_json::to_value(actual?)?,
                serde_json::to_value(&cold_actual)?
            );
            assert_eq!(bounded.finish()?, cold_usage);
        } else {
            assert!(format!("{:#}", actual.unwrap_err()).contains("ResourceExhausted"));
            assert!(bounded.finish().is_err());
            assert!(bounded.read(|| Ok(())).is_err());
        }
    }
    assert_eq!(read.acquisition.finish()?, loaded_usage);
    // Use fixture facts, not another acquisition or the implementation under
    // test, as the oracle for every public field and the original 16 window.
    let expected = ChildAgentObservabilitySnapshot {
        phase: ChildAgentPhase::Waiting,
        blocked_reason: None,
        waiting_reason: None,
        current_work_item_id: None,
        work_summary: None,
        last_progress_brief: None,
        last_result_brief: Some("original-child-brief-19".into()),
        _read_material: None,
        _source_material: None,
    };
    assert_eq!(
        serde_json::to_value(cold_actual)?,
        serde_json::to_value(&expected)?
    );
    assert_eq!(
        expected.last_progress_brief, None,
        "Ack outside the original 16 window stays excluded"
    );
    assert_eq!(
        expected.last_result_brief.as_deref(),
        Some("original-child-brief-19")
    );
    assert_eq!(read.projection.active_tasks.len(), 1);
    assert_eq!(
        read.projection.active_tasks[0].id,
        "child-observability-running"
    );
    assert_eq!(read.projection.active_tasks[0].status, TaskStatus::Running);
    assert_eq!(
        crate::runtime::task_reads::task_record(&read.tasks, "child-observability-running",)?
            .expect("actual Running Task remains selected")
            .status,
        TaskStatus::Running
    );
    // Active Tasks re-enter through terminal results and are not scheduler blockers.
    assert_eq!(expected.blocked_reason, None);
    drop(read);
    drop(mutation);
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("DROP TABLE tasks; DROP TABLE briefs;")?;
        Ok(())
    })?;
    let actual = runtime.child_agent_observability().await?;
    assert_eq!(
        serde_json::to_value(actual)?,
        serde_json::to_value(expected)?
    );
    Ok(())
}

#[tokio::test]
async fn scheduler_acquisition_matches_legacy_live_work_and_active_wait_projection() -> Result<()> {
    let (_home, runtime) = fixture();
    let work = runtime
        .create_work_item("waiting original Work".into(), None, None, Vec::new())
        .await?;
    runtime
        .register_wait_for(
            "scheduler-resident",
            Some(work.id.clone()),
            WaitForWakeKind::External,
            Some("github:holon-run/holon#2258".into()),
            "wait for external".into(),
            None,
        )
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let now = runtime.now();
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), now)
        .await?;
    let legacy = SchedulerProjection::from_state_with_queue_len_at(
        &runtime.inner.storage,
        &read.state,
        mutation.queue.len(),
        now,
    )?;
    assert_eq!(read.projection, legacy);
    assert_eq!(
        read.scheduling_posture,
        runtime.storage().agent_posture_projection(&read.state)?
    );
    assert_eq!(
        read.active_waits,
        runtime
            .storage()
            .active_wait_conditions_for_agent(&read.state.id)?
    );
    assert_eq!(
        read.advisories,
        scheduler::scheduling_advisories_with_queue_len(
            runtime.storage(),
            &read.state,
            mutation.queue.len(),
        )?
    );
    assert_eq!(read.work.live_work_items()?.len(), 1);
    assert_eq!(read.work.unresolved_wait_conditions()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn scheduler_triggered_wait_capture_and_dispatch_survive_storage_removal() -> Result<()> {
    let (_home, runtime) = fixture();
    let registered = runtime
        .register_wait_for(
            "scheduler-resident",
            None,
            WaitForWakeKind::OperatorInput,
            None,
            "operator resume".into(),
            None,
        )
        .await?;
    let message = runtime
        .enqueue(MessageEnvelope::new(
            "scheduler-resident",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("test".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "resume".into(),
            },
        ))
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let now = runtime.now();
    let read = mutation
        .prepare_scheduler_read(Some(&message), Default::default(), Default::default(), now)
        .await?;
    let legacy = SchedulerProjection::from_state_with_queue_len_at(
        &runtime.inner.storage,
        &read.state,
        mutation.queue.len(),
        now,
    )?;
    assert_eq!(read.projection, legacy);
    let waits = read.work.waits_triggered_by(&message.id)?;
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].id, registered.condition.id);
    assert_eq!(waits[0].status, WaitConditionStatus::Triggered);
    assert_eq!(read.projection.active_waiting_intents, 0);
    drop(mutation);
    // The decision, posture and advisories read only this selected capture.
    let expected_posture = runtime.storage().agent_posture_projection(&read.state)?;
    let expected_advisories = scheduler::scheduling_advisories_with_queue_len(
        runtime.storage(),
        &read.state,
        read.projection.queue_len,
    )?;
    std::fs::remove_file(runtime.inner.runtime_db.path())?;
    assert_eq!(read.scheduling_posture, expected_posture);
    assert_eq!(read.advisories, expected_advisories);
    assert!(read.active_waits.is_empty());
    let closure = RuntimeHandle::closure_decision_from_projection(
        &read.state,
        &read.projection,
        None,
        &read.acquisition,
    )?;
    let dispatch = runtime.build_message_dispatch_plan_from_work(
        &message,
        closure,
        &read.state,
        &read.work,
        &read.tasks,
    )?;
    let _decision = scheduler::decide_next_action(
        &read.projection,
        scheduler::SchedulerBoundary::RunLoop,
        scheduler::SchedulerInput::Message {
            message: &message,
            model_turn_allowed: dispatch.model_turn_allowed,
            continuation_resolution: dispatch.continuation_resolution.as_ref(),
        },
    );
    Ok(())
}

#[tokio::test]
async fn scheduler_dispatch_requires_explicit_trigger_relation_preparation() -> Result<()> {
    let (_home, runtime) = fixture();
    let mut mutation = runtime.begin_mutation().await?;
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
        .await?;
    drop(mutation);
    let message = MessageEnvelope::new(
        "scheduler-resident",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "not admitted/captured".into(),
        },
    );
    let closure = RuntimeHandle::closure_decision_from_projection(
        &read.state,
        &read.projection,
        None,
        &read.acquisition,
    )?;
    let error = match runtime.build_message_dispatch_plan_from_work(
        &message,
        closure,
        &read.state,
        &read.work,
        &read.tasks,
    ) {
        Ok(_) => anyhow::bail!("unprepared relation was silently hydrated"),
        Err(error) => error,
    };
    assert!(
        matches!(error.downcast_ref::<crate::runtime::resident_work::WorkReadError>(),
        Some(crate::runtime::resident_work::WorkReadError::TriggerWaitsNotPrepared(id)) if id == &message.id)
    );
    Ok(())
}

#[tokio::test]
async fn scheduler_prepares_current_source_after_positive_episode_recovery_invalidates_domains(
) -> Result<()> {
    use crate::{
        memory::episode_transition::{EpisodeMemoryOutcome, EpisodeMemoryPlan},
        runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor},
        runtime_db::episode_memory::{
            EpisodeMemoryFaultPoint, EpisodeMemoryInvocation, EpisodeMemoryKey,
            EpisodeMemoryTransitionCommand,
        },
    };
    let (_home, runtime) = fixture();
    let source = runtime
        .enqueue(MessageEnvelope::new(
            "scheduler-resident",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "current source after lost ACK".into(),
            },
        ))
        .await?;
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime).poll().await?
    else {
        anyhow::bail!("source was not claimed");
    };
    runtime
        .begin_interactive_turn(Some(&scheduled.message), None, None)
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let state = mutation.state.clone();
    let binding = state
        .current_execution_binding
        .clone()
        .context("actual binding missing")?;
    let memory = &state.working_memory;
    let outcome = EpisodeMemoryOutcome {
        episode_memory_changed: false,
        business_fields_changed: false,
        boundary_reason: None,
        finalized_episode_id: None,
        previous_builder_id: memory
            .active_episode_builder
            .as_ref()
            .map(|value| value.id.clone()),
        active_builder_id: memory
            .active_episode_builder
            .as_ref()
            .map(|value| value.id.clone()),
        compression_epoch_before: memory.compression_epoch,
        compression_epoch_after: memory.compression_epoch,
        archived_episode_count_before: memory.archived_episode_count,
        archived_episode_count_after: memory.archived_episode_count,
    };
    let command = EpisodeMemoryTransitionCommand::new(
        EpisodeMemoryKey {
            agent_id: state.id.clone(),
            identity_incarnation: mutation.input_evidence_scope().identity_incarnation,
            actual_dispatch_turn_id: binding.turn_id.clone(),
        },
        EpisodeMemoryInvocation {
            message_id: source.id.clone(),
            turn_index: state.turn_index,
        },
        state.clone(),
        EpisodeMemoryPlan {
            candidate: state.clone(),
            finalized_episode: None,
            audit_events: Vec::new(),
            outcome,
        },
        runtime.now(),
    )?
    .with_fault(EpisodeMemoryFaultPoint::AfterCommitBeforeAck);
    let error = Box::pin(mutation.commit_episode_memory(&command))
        .await
        .unwrap_err();
    assert_eq!(
        crate::runtime_db::persistence_certainty(&error),
        Some(crate::runtime_db::PersistenceCertainty::Indeterminate)
    );
    drop(mutation);
    // begin_mutation performs real receipt reconciliation. Episode recovery
    // deliberately publishes current State/Episodes without guessing Inputs/Work.
    let mut mutation = runtime.begin_mutation().await?;
    let root = mutation.capture_published_root()?;
    assert!(root.input_evidence.is_none() && root.work.is_none());
    assert_eq!(
        root.state.current_execution_binding.as_ref(),
        Some(&binding)
    );
    drop(root);
    // Model an incomplete recovered publication below the owner preparation
    // boundary. Ordinary capture must not repair its missing scheduler window.
    let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
    let narrow = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        query: None,
        windows: vec![
            crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                selector: InputWindowSelector::RecentBriefs,
                limit: 1,
            },
        ],
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit,
        byte_limit,
    };
    let prepared = runtime.inner.runtime_db.transaction(|tx| {
        crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
            tx,
            &mutation.input_evidence_scope(),
            &mutation.state,
            &narrow,
            context_config().recent_episode_candidates,
            &runtime.inner.input_evidence_budget,
            &crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
        )
    })?;
    mutation.input_evidence = Some(Arc::new(
        prepared
            .inputs
            .with_budget(runtime.inner.input_evidence_budget.clone())?,
    ));
    mutation.scheduling = Some(prepared.scheduling);
    mutation.install_transient().await?;
    mutation
        .prepare_owner_work_request(&crate::runtime::resident_work::WorkCaptureRequest::current())
        .await?;
    mutation
        .prepare_owner_task_request(&crate::runtime::task_reads::active_task_request())
        .await?;
    let before = mutation.capture_published_root()?;
    let alias_rows = || -> Result<Vec<(String, String, i64, Option<String>, String, String)>> {
        let connection = runtime.inner.runtime_db.connection()?;
        let mut query = connection.prepare("SELECT family,record_id,revision,content_digest,availability,fingerprint_json FROM input_evidence_current WHERE agent_id=?1 ORDER BY family,record_id")?;
        let rows = query
            .query_map([&source.agent_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    let aliases_before = alias_rows()?;
    let error = mutation
        .capture_scheduler_read(
            &crate::runtime::resident_work::WorkCaptureRequest::current(),
            runtime.now(),
        )
        .err()
        .context("ordinary read silently expanded the Brief window")?;
    assert_eq!(
        error.downcast_ref::<crate::runtime::resident_input_evidence::InputEvidenceReadError>(),
        Some(&crate::runtime::resident_input_evidence::InputEvidenceReadError::WindowNotPrepared),
        "{error:#}"
    );
    assert_eq!(mutation.capture_published_root()?.token, before.token);
    assert_eq!(alias_rows()?, aliases_before);
    drop(before);
    let read = mutation
        .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
        .await?;
    assert_eq!(
        read.state.current_execution_binding.as_ref(),
        Some(&binding)
    );
    let inputs = mutation.capture_input_evidence()?;
    let actual = crate::runtime::provider_recovery::captured_message(&inputs, &source.id)?
        .context("source was not explicitly prepared")?;
    assert_eq!(actual.id, source.id);
    assert!(actual.message_seq.is_some());
    assert_eq!(
        inputs.message_turn_keys.get(&source.id),
        Some(&Some(binding.turn_id.clone()))
    );
    drop(scheduled);
    Ok(())
}

#[tokio::test]
async fn activation_prepares_original_closure_brief_policy_without_ordinary_hydration() -> Result<()>
{
    let (_home, runtime) = fixture();
    let mutation = runtime.begin_mutation().await?;
    let before = mutation.capture_published_root()?;
    let now = runtime.now();
    let read = mutation.capture_scheduler_read(
        &crate::runtime::resident_work::WorkCaptureRequest::current(),
        now,
    )?;
    let legacy = SchedulerProjection::from_state_with_queue_len_at(
        runtime.storage(),
        &read.state,
        mutation.queue.len(),
        now,
    )?;
    assert_eq!(read.projection, legacy);
    assert_eq!(
        read.scheduling_posture,
        runtime.storage().agent_posture_projection(&read.state)?
    );
    assert!(mutation
        .capture_input_evidence()?
        .window(
            &InputWindowSelector::RecentBriefs,
            scheduler::SCHEDULER_BRIEF_LIMIT,
        )?
        .is_empty());
    assert_eq!(mutation.capture_published_root()?.token, before.token);
    Ok(())
}
