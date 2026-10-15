use super::*;
use crate::runtime::{
    current_read_facts::CurrentReadError, tests::support::LifecycleHarness, WaitForWakeKind,
};
use crate::types::{
    AuditEvent, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    TaskKind, TaskStatus,
};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
fn deny(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO projection_forbidden_states;
         ALTER TABLE tasks RENAME TO projection_forbidden_tasks;
         ALTER TABLE work_items RENAME TO projection_forbidden_work;
         ALTER TABLE wait_conditions RENAME TO projection_forbidden_waits;
         ALTER TABLE timers RENAME TO projection_forbidden_timers;
         ALTER TABLE timer_wakes RENAME TO projection_forbidden_wakes;
         ALTER TABLE external_triggers RENAME TO projection_forbidden_triggers;
         ALTER TABLE messages RENAME TO projection_forbidden_messages;
         ALTER TABLE queue_entries RENAME TO projection_forbidden_queue;
         ALTER TABLE audit_events RENAME TO projection_forbidden_events;",
    )?;
    Ok(())
}
fn exhausted(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::ResourceExhausted)
    ) || matches!(
        error.downcast_ref::<super::super::super::resident_task::TaskReadError>(),
        Some(super::super::super::resident_task::TaskReadError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<super::super::super::resident_input_evidence::InputCaptureError>(),
        Some(super::super::super::resident_input_evidence::InputCaptureError::ResourceExhausted)
    ) || matches!(
        error.downcast_ref::<super::super::super::resident_scheduling::SchedulingReadError>(),
        Some(super::super::super::resident_scheduling::SchedulingReadError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<super::super::super::resident_work::WorkReadError>(),
        Some(super::super::super::resident_work::WorkReadError::ResourceExhausted(_))
    )
}

#[tokio::test]
async fn ordinary_scheduler_uses_original_projection_and_admits_actual_prior_before_every_owned_copy(
) -> Result<()> {
    // Match LifecycleHarness's original clock/provider/workspace, with only
    // this finite fixture's resource home isolated from the developer HOME.
    let data_dir = crate::runtime::tests::support::tempdir()?;
    let workspace = crate::runtime::tests::support::tempdir()?;
    let runtime = RuntimeHandle::new_with_clock_without_user_home_for_test(
        "default",
        data_dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(crate::provider::StubProvider::new("unused")),
        "default".into(),
        crate::runtime::tests::support::context_config(),
        crate::runtime::tests::support::controlled_clock(),
    )?;
    let runtime = &runtime;
    let work = runtime
        .create_work_item(
            "ordinary original scheduler Work".into(),
            None,
            None,
            Vec::new(),
        )
        .await?;
    runtime
        .register_wait_for(
            runtime.instance_agent_id(),
            Some(work.id),
            WaitForWakeKind::External,
            Some("actual-original-external".into()),
            "wait for actual source".into(),
            None,
        )
        .await?;
    let mut held = runtime.begin_mutation().await?;
    let now = runtime.now();
    let setup = held
        .prepare_scheduler_read(None, Default::default(), Default::default(), now)
        .await?;
    let expected = SchedulerProjection::from_state_with_queue_len_at(
        runtime.storage(),
        &setup.state,
        held.queue.len(),
        now,
    )?;
    assert_eq!(setup.projection, expected);
    let expected_queue = serde_json::to_value(&setup.work_queue.items)?;
    let expected_posture = serde_json::to_value(&setup.scheduling_posture)?;
    drop((setup, held));
    // All original Work/Wait resource preparation has finished. Only the
    // following pure selection/measurement phase may stop the observer.
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Root absent")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let prior = (128 * 1024u64, 32usize);
    let prior_guard = ledger.register_owned_bytes(
        root.tasks.as_ref().unwrap().scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let prior_body = vec![0u8; usize::try_from(prior.0)?];
    let held_bytes = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    deny(runtime)?;
    let request = WorkCaptureRequest::current();
    let read = runtime.capture_scheduler_read_from_snapshot_with_material(
        &root, &request, now, LIMITS, prior, None,
    )?;
    assert_eq!(read.projection, expected);
    assert_eq!(
        serde_json::to_value(&read.work_queue.items)?,
        expected_queue
    );
    assert_eq!(
        serde_json::to_value(&read.scheduling_posture)?,
        expected_posture
    );
    assert_eq!(
        read.material_estimate()?,
        read.additional_material_estimate()?
    );
    let cost = read.additional_material_estimate()?;
    assert!(cost.0 > 0 && cost.1 > 0);
    let weak = Arc::downgrade(&read._owned_budget);
    let read = Arc::new(read);
    let final_holder = read.clone();
    drop(read);
    assert!(weak.upgrade().is_some());
    assert_eq!(Arc::strong_count(&root), strong);
    drop(final_holder);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, held_bytes);
    let fits = |limits| -> Result<bool> {
        let result = runtime.capture_scheduler_read_from_snapshot_with_material(
            &root, &request, now, limits, prior, None,
        );
        let fits = match result {
            Ok(read) => {
                assert_eq!(read.projection, expected);
                drop(read);
                true
            }
            Err(error) => {
                assert!(exhausted(&error), "{error:#}");
                false
            }
        };
        assert_eq!(ledger.report()?.observed_bytes, held_bytes);
        assert_eq!(Arc::strong_count(&root), strong);
        Ok(fits)
    };
    let mut bytes = (1usize, LIMITS.0);
    while bytes.0 < bytes.1 {
        let middle = bytes.0 + (bytes.1 - bytes.0) / 2;
        if fits((middle, LIMITS.1))? {
            bytes.1 = middle
        } else {
            bytes.0 = middle + 1
        }
    }
    let mut nodes = (1usize, LIMITS.1);
    while nodes.0 < nodes.1 {
        let middle = nodes.0 + (nodes.1 - nodes.0) / 2;
        if fits((bytes.0, middle))? {
            nodes.1 = middle
        } else {
            nodes.0 = middle + 1
        }
    }
    // The final native limit itself is part of Current's borrowed metadata.
    // Re-measure bytes with that exact number, preserving both strict edges.
    let mut joint_bytes = (1usize, bytes.0);
    while joint_bytes.0 < joint_bytes.1 {
        let middle = joint_bytes.0 + (joint_bytes.1 - joint_bytes.0) / 2;
        if fits((middle, nodes.0))? {
            joint_bytes.1 = middle
        } else {
            joint_bytes.0 = middle + 1
        }
    }
    bytes.0 = joint_bytes.0;
    assert!(!fits((bytes.0 - 1, nodes.0))?);
    assert!(!fits((bytes.0, nodes.0 - 1))?);
    assert!(fits((bytes.0, nodes.0))?);
    assert_eq!(prior_body.len(), usize::try_from(prior.0)?);
    drop((prior_body, prior_guard));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

async fn actual_sources(runtime: &RuntimeHandle) -> Result<()> {
    let mut held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    let scope = root.tasks.as_ref().context("actual scope missing")?.scope();
    let roles = held.input_query_roles(scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    let events = held.current_event_roles(scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    // Ordinary Scheduler needs its original RecentBriefs window in addition
    // to the actual birth role. Qualify both through the same named SQL actor.
    let mut windows = Vec::new();
    super::super::super::scheduler::include_scheduler_brief_window(&mut windows);
    let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows,
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: LIMITS.0,
        record_limit: LIMITS.1,
    };
    let (input, current) = crate::runtime_db::input_evidence_snapshot::prepare_named_run_sources_with_request_for_fixture(
        runtime.runtime_db(),
        scope,
        &runtime.inner.input_evidence_budget,
        roles.as_ref(),
        held.owner_epoch(),
        &root.state,
        events.as_ref(),
        &request,
    )?;
    held.begin("ordinary-scheduler-actual-source-preparation".into())?;
    held.committed_prepared_with_current_read_domains(
        root.state.clone(),
        root.episodes.clone(),
        Some(Arc::new(input)),
        root.work.clone(),
        root.tasks.clone(),
        root.scheduling.clone(),
        Some(current),
    )?;
    held.publish(root.state.clone())?;
    Ok(())
}
#[tokio::test]
async fn ordinary_scheduler_source_loans_discount_only_original_bodies_and_keep_fresh_dtos_to_last_holder(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    actual_sources(runtime).await?;
    let sessions = runtime.begin_run_birth_sessions().await?;
    let at = runtime.now();
    runtime
        .persist_task_transition(
            &TaskRecord {
                id: "ordinary-source-task".into(),
                agent_id: runtime.instance_agent_id().into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Queued,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: None,
                detail: Some(serde_json::json!({"actual":[0,1,2]})),
                recovery: None,
            },
            "task_queued",
        )
        .await?;
    runtime
        .enqueue(MessageEnvelope::new(
            runtime.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "actual born input".into(),
            },
        ))
        .await?;
    runtime
        .append_owned_runtime_audit_event(AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({"actual":true}),
        ))
        .await?;
    actual_sources(runtime).await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Root absent")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let born = runtime.capture_run_birth_sessions_from_snapshot(&sessions, &root, LIMITS)?;
    let prior = born.material_estimate()?;
    let loans = SchedulerReadBodyLoans::from_run_birth(runtime, &root, &born)?;
    let other = LifecycleHarness::new();
    assert!(SchedulerReadBodyLoans::from_run_birth(other.runtime(), &root, &born).is_err());
    let strong = Arc::strong_count(&root);
    deny(runtime)?;
    let now = runtime.now();
    let request = WorkCaptureRequest::current();
    let read = runtime.capture_scheduler_read_from_snapshot_with_material(
        &root,
        &request,
        now,
        LIMITS,
        prior,
        Some(&loans),
    )?;
    let own = read.material_estimate()?;
    let added = read.additional_material_estimate()?;
    assert!(own.0 > added.0 && own.1 > added.1);
    assert!(added.0 > read._owned_budget.owned_material_bytes()?);
    assert_eq!(read.projection.active_tasks.len(), 1);
    assert_eq!(read.projection.active_tasks[0].id, "ordinary-source-task");
    assert!(runtime
        .capture_scheduler_read_from_snapshot_with_material(
            &root,
            &request,
            now,
            LIMITS,
            (prior.0 - 1, prior.1),
            Some(&loans)
        )
        .is_err());
    assert_eq!(Arc::strong_count(&root), strong);
    let weak = Arc::downgrade(&read._owned_budget);
    drop(loans);
    drop(born);
    assert!(weak.upgrade().is_some());
    drop(read);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
