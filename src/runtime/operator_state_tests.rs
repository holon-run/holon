use super::*;
use crate::runtime::tests::support::LifecycleHarness;
use crate::types::WorkItemPlanStatus;

#[tokio::test]
async fn loaded_state_first_read_and_commits_do_not_acquire_storage() -> Result<()> {
    async fn read(runtime: &RuntimeHandle) -> Result<crate::host::AgentStateReadProjection> {
        let db = runtime.runtime_db().clone();
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let reader_before = db.reader_pool_stats_for_tests().checkout_attempts;
        let preparation_before = db.preparation_pool_stats_for_tests().checkout_attempts;
        let writer = db.reader_pool_writer_weak_for_tests().upgrade().unwrap();
        let writes_before = writer.write_channel_admissions_for_tests()?;
        let runtime_reader = runtime.clone();
        let handle = tokio::runtime::Handle::current();
        // A memory-only observer may read a Ready root while its owner is held.
        let _reservation = runtime.inner.mutation_owner.reserve().await?;
        let projection = tokio::task::spawn_blocking(move || {
            runtime_reader.acquire_operator_state_read_projection_scoped(10, 10, &db, &handle)
        })
        .await??;
        let db = runtime.runtime_db();
        assert_eq!(
            db.reader_pool_stats_for_tests().checkout_attempts,
            reader_before
        );
        assert_eq!(
            db.preparation_pool_stats_for_tests().checkout_attempts,
            preparation_before
        );
        assert_eq!(writer.write_channel_admissions_for_tests()?, writes_before);
        assert!(Arc::ptr_eq(
            &root,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));
        Ok(projection)
    }

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    assert!(read(runtime).await?.agent.work_queue.items.is_empty());
    let work = runtime
        .create_work_item(
            "State writer coverage".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await?;
    assert_eq!(
        read(runtime).await?.agent.work_queue.items[0].work_item.id,
        work.id
    );
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    assert_eq!(read(runtime).await?.timers[0].id, timer.id);
    runtime.cancel_timer(&timer.id).await?;
    assert!(read(runtime).await?.timers.is_empty());
    runtime
        .append_original_audit_event(crate::types::AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({}),
        ))
        .await?;
    drop(read(runtime).await?);
    Ok(())
}

#[tokio::test]
async fn state_capture_needs_only_its_published_dependencies() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut selected = root.as_ref().clone();
    selected.current = None;
    selected.episodes = None;
    drop(runtime.operator_state_from_root(&selected, 10, 10)?);
    for missing in 0..8 {
        let mut incomplete = selected.clone();
        match missing {
            0 => incomplete.identity = None,
            1 => incomplete.queue_posture = None,
            2 => incomplete.scheduler_events = None,
            3 => incomplete.work = None,
            4 => incomplete.tasks = None,
            5 => incomplete.scheduling = None,
            6 => incomplete.input_evidence = None,
            _ => incomplete.token.identity_incarnation += 1,
        }
        let error = runtime
            .operator_state_from_root(&incomplete, 10, 10)
            .err()
            .unwrap();
        if missing == 7 {
            assert!(error.to_string().contains("State root scope differs"));
        } else {
            assert!(
                error.is::<crate::runtime_db::RuntimeDbRetryableError>(),
                "{error:#}"
            );
        }
        assert!(Arc::ptr_eq(
            &root,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));
    }
    Ok(())
}

#[tokio::test]
async fn loaded_state_surfaces_keep_full_queue_beyond_scheduler_candidates() -> Result<()> {
    fn queue_wire<W: serde::Serialize>(
        queue: &crate::work_item_scheduling::WorkQueueReadModel<W>,
    ) -> Result<serde_json::Value> {
        Ok(serde_json::to_value((
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
        ))?)
    }

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    for index in 0..9 {
        runtime
            .create_work_item(
                format!("full State inventory {index}"),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
    }
    let read = runtime.capture_scheduler_projection().await?;
    assert_eq!(read.projection.queued_runnable_work_items.len(), 5);
    let full = read.work.work_queue_with_material(
        &read.state,
        &read.triggers.records,
        &read.acquisition,
    )?;
    assert_eq!(full.items.len(), 9);
    let expected = queue_wire(&full)?;
    let lightweight = runtime.lightweight_agent_state_projection().await?;
    assert_eq!(queue_wire(&lightweight.work_queue)?, expected);
    assert!(lightweight._work_budget.is_some());
    let operator = runtime.operator_state_read_projection(10, 10).await?;
    assert_eq!(queue_wire(&operator.agent.work_queue)?, expected);
    assert!(operator.material._work.is_some());

    // The full-inventory adapter keeps its independent Current preparation.
    // State above already used only the minimal published root.
    let root = runtime
        .capture_observer_root_async(runtime.runtime_db())
        .await?;
    let mut read = runtime.capture_operator_scheduler_from_root(&root, runtime.now())?;
    let usage = read.acquisition.finish()?;
    let (queue, budget) = read.full_state_work_queue(&runtime.inner.input_evidence_budget)?;
    assert_eq!(queue_wire(&queue)?, expected);
    assert_eq!(
        read.acquisition.finish()?,
        usage,
        "moving the operator inventory must not allocate or rank it again"
    );
    drop((read, queue, budget));

    let ledger = &runtime.inner.input_evidence_budget;
    let scope = root.work.as_ref().unwrap().scope();
    for (bytes, records, succeeds) in [
        (usage.0, usage.1, true),
        (usage.0 - 1, usage.1, false),
        (usage.0, usage.1 - 1, false),
    ] {
        let phase =
            resource_io::ResourceReadMaterial::with_test_limits(ledger, scope, bytes, records)?;
        let result =
            phase.read(|| runtime.capture_operator_scheduler_from_root(&root, runtime.now()));
        if succeeds {
            let mut read = result?;
            assert!(phase.shares_allowance(&read.acquisition));
            let (queue, _budget) = read.full_state_work_queue(ledger)?;
            assert_eq!(queue_wire(&queue)?, expected);
            assert_eq!(phase.finish()?, usage);
        } else {
            resource_io::assert_retryable_resource_error(&result.err().unwrap());
            assert!(phase.finish().is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn operator_state_keeps_scheduler_window_after_narrow_owner_preparation() -> Result<()> {
    use crate::domain::execution_protocol::{ExecutionAttemptState, WorkItemExecutionState};
    use crate::runtime::resident_input_evidence::{
        InputEvidenceFamily, InputEvidenceKey, InputEvidenceScope, InputWindowSelector,
    };
    use crate::runtime_db::input_evidence_snapshot::{
        InputEvidenceLoadRequest, InputWindowRequest,
    };
    use crate::types::{
        AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, Priority,
    };

    for limit in [0, 1] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let mut work_ids = Vec::new();
        for index in 0..3 {
            work_ids.push(
                runtime
                    .create_work_item(
                        format!("live scheduler frontier {index}"),
                        Some(WorkItemPlanStatus::Ready),
                        None,
                        Vec::new(),
                    )
                    .await?
                    .id,
            );
        }
        let waiting = runtime
            .register_wait_for(
                "default",
                Some(work_ids[2].clone()),
                WaitForWakeKind::External,
                Some("scheduler-frontier-fixture".into()),
                "retain the waiting execution frontier".into(),
                None,
            )
            .await?;
        let waiter = waiting.work_item.as_ref().unwrap();
        // Registration without a live attempt does not settle an execution.
        crate::runtime::tests::support::seed_live_waiting_work_execution(
            runtime,
            waiter,
            &waiting.condition.id,
        )
        .await;
        runtime.pick_work_item(work_ids[0].clone()).await?;
        let mut source = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "admit a live scheduler frontier".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::HttpControlPrompt,
            AdmissionContext::ControlAuthenticated,
        );
        source.work_item_id = Some(work_ids[0].clone());
        let source = runtime.enqueue(source).await?;
        let scheduler_executor::RunLoopPoll::Message(scheduled) =
            scheduler_executor::SchedulerDecisionExecutor::new(runtime)
                .poll()
                .await?
        else {
            panic!("the frontier fixture must admit its source");
        };
        assert_eq!(scheduled.message.id, source.id);
        runtime
            .begin_dispatch_turn(
                &scheduled.message,
                None,
                None,
                scheduled
                    .dispatch_plan
                    .execution_admission_provenance
                    .clone(),
                &scheduled.dispatch_plan.prior_closure,
            )
            .await?;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        drop(runtime.operator_state_read_projection(10, 10).await?);
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let message = runtime.runtime_db().transaction(|tx| {
            let message = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "qualified narrow preparation dependency".into(),
                },
            );
            let (stored, inserted) = crate::runtime_db::evidence::append_message_tx(tx, &message)?;
            assert!(inserted);
            crate::runtime_db::input_evidence_snapshot::register_committed_input_evidence_tx(
                tx,
                &InputEvidenceScope {
                    agent_id: "default".into(),
                    identity_incarnation: runtime.inner.identity_incarnation,
                },
                &[
                    crate::runtime_db::input_evidence_snapshot::CanonicalInputEvidence::Message(
                        stored.clone(),
                    ),
                ],
            )?;
            Ok(stored)
        })?;
        let mut mutation = runtime.begin_mutation().await?;
        let (_, record_limit, byte_limit) = mutation.context_input_evidence_policy()?;
        let request = InputEvidenceLoadRequest {
            query: None,
            windows: if limit == 0 {
                Vec::new()
            } else {
                vec![InputWindowRequest {
                    selector: InputWindowSelector::RecentBriefs,
                    limit,
                }]
            },
            mandatory_roots: std::collections::BTreeSet::from([InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                &message.id,
            )]),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        mutation.prepare_owner_input_request(&request, 0).await?;
        let prepared = mutation.capture_published_root()?;
        assert_ne!(prepared.token, before.token, "exercise actual preparation");
        let key = InputEvidenceKey::new(InputEvidenceFamily::Message, &message.id);
        assert!(prepared
            .input_evidence
            .as_ref()
            .unwrap()
            .capture()?
            .lookup(&key)?
            .is_some());
        let read = runtime.capture_state_scheduler_from_root(&prepared, runtime.now())?;
        assert!(
            matches!(
                read.inputs.lookup(&key),
                Err(crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(missing))
                    if missing == key
            ),
            "State must neither capture nor qualify absence of an unrelated owner Message"
        );
        let execution = read
            .inputs
            .execution_state_if_initialized()?
            .expect("the fixture initialized a nonempty execution frontier");
        assert_eq!(execution.attempts.len(), 1);
        assert!(execution.attempts.values().all(|attempt| {
            attempt.state == ExecutionAttemptState::Open
                && attempt.source_message_id.as_deref() == Some(source.id.as_str())
        }));
        for (id, expected) in work_ids.iter().zip(["in_flight", "runnable", "waiting"]) {
            let state = &execution.work_items[id].state;
            assert!(
                matches!(
                    (expected, state),
                    ("in_flight", WorkItemExecutionState::InFlight { .. })
                        | ("runnable", WorkItemExecutionState::Runnable { .. })
                        | ("waiting", WorkItemExecutionState::Waiting { .. })
                ),
                "the fixture must exercise {expected}: {state:?}"
            );
        }
        assert!(read
            .inputs
            .lookup(&InputEvidenceKey::new(
                InputEvidenceFamily::Message,
                &source.id
            ))?
            .is_some());
        assert_eq!(
            Some(execution),
            prepared
                .input_evidence
                .as_ref()
                .unwrap()
                .capture()?
                .execution_state_if_initialized()?,
            "narrow Input selection must retain the live execution frontier"
        );
        drop(read);
        drop((mutation, prepared, before));

        // The publication, not an ordinary State read, must retain its dependencies.
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE messages RENAME TO narrow_state_messages_forbidden;
             ALTER TABLE briefs RENAME TO narrow_state_briefs_forbidden;",
            )?;
            Ok(())
        })?;
        let projection = runtime.operator_state_read_projection(10, 10).await?;
        assert_eq!(projection.agent.agent.id, "default");
    }
    Ok(())
}

#[tokio::test]
async fn operator_state_metadata_uses_original_window_without_granting_event_bodies() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    for index in 0..64 {
        runtime
            .inner
            .storage
            .append_event(&crate::types::AuditEvent::legacy(
                if index == 0 {
                    "runtime_error"
                } else {
                    "metadata_only"
                },
                serde_json::json!({"raw":"no original body qualification"}),
            ))?;
    }
    // Raw storage writes bypass publication. Only the independent Current
    // preparation may repair this fixture; State must not perform that work.
    assert!(runtime
        .operator_state_read_projection(10, 10)
        .await
        .is_err());
    let root = runtime
        .capture_observer_root_async(runtime.runtime_db())
        .await?;
    let query = root.current.as_ref().unwrap().query.as_ref().unwrap();
    assert!(query.scheduler.latest_runtime_error.is_some());
    assert!(
        query
            .capture(
                &crate::runtime_db::current_query::CurrentSelection {
                    events_after: Some(0),
                    ..Default::default()
                },
                &runtime.inner.input_evidence_budget,
            )
            .is_err(),
        "metadata must not grant raw Event bodies"
    );
    drop(root);
    runtime
        .append_original_audit_event(crate::types::AuditEvent::legacy(
            "metadata_only",
            serde_json::json!({}),
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(
        root.current
            .as_ref()
            .and_then(|current| current.query.as_ref())
            .is_none(),
        "a metadata-only append must not grant CurrentQuery"
    );
    assert_eq!(
        root.scheduler_events.unwrap().latest_runtime_error,
        None,
        "the 65th event must move the old error outside the original window"
    );
    drop(root);
    runtime.operator_state_read_projection(10, 10).await?;
    Ok(())
}

#[tokio::test]
async fn operator_state_loaded_same_cut_needs_no_body_sql_or_old_root() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("State cut".into()))
        .await?;
    let first = runtime.operator_state_read_projection(10, 10).await?;
    assert_eq!(first.timers[0].id, timer.id);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(crate::types::AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({}),
        ))
        .await?;
    assert!(old.upgrade().is_none(), "projection must not retain a Root");
    // Body repositories are unavailable after preparation.
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE agent_states RENAME TO state_endpoint_states_forbidden;
             ALTER TABLE agent_identities RENAME TO state_endpoint_identity_forbidden;
             ALTER TABLE queue_entries RENAME TO state_endpoint_queue_forbidden;
             ALTER TABLE audit_events RENAME TO state_endpoint_events_forbidden;
             ALTER TABLE tasks RENAME TO state_endpoint_tasks_forbidden;
             ALTER TABLE wait_conditions RENAME TO state_endpoint_waits_forbidden;
             ALTER TABLE timers RENAME TO state_endpoint_timers_forbidden;
             ALTER TABLE timer_wakes RENAME TO state_endpoint_wakes_forbidden;
             ALTER TABLE external_triggers RENAME TO state_endpoint_triggers_forbidden;",
        )?;
        Ok(())
    })?;
    let _agent = runtime.inner.agent.lock().await;
    let next = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.operator_state_read_projection(10, 10),
    )
    .await??;
    assert_eq!(next.agent.agent, first.agent.agent);
    assert_eq!(next.agent.identity, first.agent.identity);
    assert_eq!(next.timers, first.timers);
    assert_eq!(next.agent.active_task_count, next.tasks.len());
    let guard = next.material.acquisition.guard()?;
    let weak = Arc::downgrade(&guard);
    drop(guard);
    let timer_holder = next.timers[0].clone();
    drop((next.timers, next.external_triggers));
    let bytes = next.material.into_json_bytes(&next.agent.agent)?;
    let cached = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    drop(bytes);
    drop(next.agent);
    assert!(weak.upgrade().is_some());
    drop(cached);
    assert!(weak.upgrade().is_some());
    drop(slice);
    assert!(
        weak.upgrade().is_some(),
        "detached Timer loan is still live"
    );
    drop(timer_holder);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn operator_state_ready_root_rejects_foreign_scope_before_projection() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    drop(runtime.operator_state_read_projection(10, 10).await?);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut foreign = root.as_ref().clone();
    foreign.token.identity_incarnation += 1;
    let error = runtime
        .operator_state_from_root(&foreign, 10, 10)
        .err()
        .context("foreign State root must fail")?;
    assert!(error.to_string().contains("State root scope differs"));
    Ok(())
}

#[tokio::test]
async fn operator_state_json_rejects_one_byte_short_without_mutating_the_prior_view() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let scope = super::super::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.agent_id().await?,
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let value = serde_json::json!({"state":"bounded original bytes"});
    let size = super::super::input_evidence_budget::serialized_bytes(&value)?;
    let holder = u64::try_from(std::mem::size_of::<StateWire>() + 128)?;
    let make = |limit| -> Result<_> {
        Ok(StateProjectionMaterial {
            acquisition: ResourceReadMaterial::with_test_limits(ledger, &scope, limit, 128)?,
            _work: None,
            _tasks: None,
            _scheduling: None,
        })
    };
    let phase = make(8192)?;
    let used = phase.acquisition.finish()?.0;
    drop(phase);
    let required = used + (holder + size) * 8;
    let good = make(required)?.into_json_bytes(&value)?;
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&good)?, value);
    let bad = make(required - 1)?;
    let failed = bad.acquisition.clone();
    assert!(bad.into_json_bytes(&value).is_err());
    assert!(
        failed.finish().is_err(),
        "the rejected original phase remains latched"
    );
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&good)?, value);
    Ok(())
}
