use super::*;
use crate::runtime::tests::support::LifecycleHarness;

#[tokio::test]
async fn default_get_agent_does_not_read_mutable_state_for_cleanup_identity() -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut owner = runtime.inner.mutation_owner.reserve().await?;
    owner.begin("pending-default-get-agent".into())?;
    let agent = runtime.inner.agent.lock().await;
    let tree_error = tokio::time::timeout(Duration::from_secs(1), host.operator_agent_tree())
        .await?
        .err()
        .context("pending tree summary must remain typed")?;
    assert!(tree_error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert!(crate::tool::ToolError::from_anyhow(&tree_error).retryable);
    let list_error = tokio::time::timeout(Duration::from_secs(1), host.list_agents())
        .await?
        .err()
        .context("pending list summary must remain typed")?;
    assert!(list_error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    let call = crate::tool::ToolCall {
        id: "pending-self".into(),
        name: "GetAgent".into(),
        input: serde_json::json!({}),
    };
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let mut read = Box::pin(registry.execute(
        &runtime,
        "default",
        &AuthorityClass::OperatorInstruction,
        &call,
    ));
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut read)
        .await
        .is_err());
    drop(agent);
    owner.definitely_not_committed()?;
    drop(owner);
    let (result, _) = tokio::time::timeout(Duration::from_secs(5), read).await??;
    assert!(!result.is_error());
    assert_eq!(
        result.envelope.result.unwrap()["agent"]["identity"]["agent_id"],
        runtime.inner.mutation_owner.agent_id()
    );

    let mut owner = runtime.inner.mutation_owner.reserve().await?;
    owner.begin("pending-default-get-agent-exhaustion".into())?;
    let agent = runtime.inner.agent.lock().await;
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        registry.execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
        ),
    )
    .await?
    .err()
    .context("pending default summary must remain retryable")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert!(runtime.inner.mutation_owner.capture().is_err());
    drop(agent);
    owner.definitely_not_committed()?;
    drop(owner);
    drop(refresh);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn runtime_activity_uses_one_observer_cut_without_execution_locks() -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prepare = runtime.begin_mutation().await?;
    prepare.prepare_current_scheduler_query(None).await?;
    drop(prepare);
    let expected = runtime.child_agent_summary_observation().await?;
    let mut owner = runtime.inner.mutation_owner.reserve().await?;
    let agent = runtime.inner.agent.lock().await;
    let activity = tokio::time::timeout(
        Duration::from_secs(1),
        host.public_agent_activity_snapshots(),
    )
    .await??;
    let current = activity
        .iter()
        .find(|entry| entry.agent_id == expected.state.id)
        .context("default activity must be visible")?;
    assert_eq!(current.status, expected.state.status);
    assert_eq!(current.active_task_count, expected.active_task_count);
    assert_eq!(
        current.last_runtime_failure,
        expected.state.last_runtime_failure
    );
    owner.begin("pending-runtime-activity".into())?;
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        host.public_agent_activity_snapshots(),
    )
    .await?
    .err()
    .context("pending activity must not wait or reconcile")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert!(runtime.inner.mutation_owner.capture().is_err());
    drop(agent);
    owner.definitely_not_committed()?;
    drop(owner);
    drop(refresh);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn get_agent_retries_a_pending_target_without_reconciling_its_owner() -> Result<()> {
    let (_home, host, parent) = crate::runtime::tests::support::host_backed_test_runtime().await;
    host.create_named_agent("summary-target", None).await?;
    let target = host.get_or_create_agent("summary-target").await?;
    target.await_initial_resources().await;
    let refresh = target.hold_resource_refresh_for_test().await?;
    let mut owner = target.inner.mutation_owner.reserve().await?;
    owner.begin("pending-get-agent-target".into())?;
    let error = parent
        .agent_summary_for("summary-target")
        .await
        .err()
        .context("a pending target must not return a summary")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    let wire = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(wire.kind, "runtime_db_busy");
    assert!(wire.retryable);
    let call = crate::tool::ToolCall {
        id: "pending-target".into(),
        name: "GetAgent".into(),
        input: serde_json::json!({"agent_id":"summary-target"}),
    };
    let mut read = Box::pin(crate::tool::tools::execute_builtin_tool(
        &parent,
        "default",
        &AuthorityClass::OperatorInstruction,
        &call,
    ));
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut read)
        .await
        .is_err());
    assert!(target.inner.mutation_owner.capture().is_err());
    owner.definitely_not_committed()?;
    drop(owner);
    let result = tokio::time::timeout(Duration::from_secs(5), read).await??;
    assert!(!result.is_error());
    assert_eq!(
        result.envelope.result.unwrap()["agent"]["identity"]["agent_id"],
        "summary-target"
    );

    let mut owner = target.inner.mutation_owner.reserve().await?;
    owner.begin("get-agent-retry-exhaustion".into())?;
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        crate::tool::tools::execute_builtin_tool(
            &parent,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
        ),
    )
    .await?
    .err()
    .context("pending target must exhaust the retry budget")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert!(crate::tool::ToolError::from_anyhow(&error).retryable);
    assert!(target.inner.mutation_owner.capture().is_err());
    owner.definitely_not_committed()?;
    drop(owner);
    drop(refresh);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn pinned_summary_and_child_cut_survive_a_new_pending_mutation() -> Result<()> {
    use crate::{
        diagnostics::io_probe::DenyResourceReaders, runtime::mutation_owner::MutationReservation,
        runtime_db::RuntimeDbReadScope,
    };
    use std::time::{Duration, Instant};

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prepare = runtime.begin_mutation().await?;
    prepare.prepare_current_scheduler_query(None).await?;
    drop(prepare);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let (_, _, _, original_resources) = runtime.summary_scheduler_from_root(&root)?;
    let mut owners = vec![runtime.inner.mutation_owner.reserve().await?];
    MutationReservation::begin_all(&mut owners, "pending-after-summary-capture".into())?;
    let denied = DenyResourceReaders::new();

    let (_, identity, _, resources) = runtime.summary_scheduler_from_root(&root)?;
    assert_eq!(identity.agent_id, root.state.id);
    assert!(original_resources
        .observation_weak()
        .ptr_eq(&resources.observation_weak()));
    let child = runtime.child_summary_from_root(&root, runtime.runtime_db())?;
    assert_eq!(child.state, root.state);
    assert_eq!(denied.attempts(), 0);
    assert!(runtime.inner.mutation_owner.capture().is_err());

    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(5));
    scope.cancel();
    let cancelled = runtime.runtime_db().with_read_scope(scope);
    assert!(runtime.child_summary_from_root(&root, &cancelled).is_err());
    MutationReservation::definitely_not_committed_all(&mut owners)?;
    Ok(())
}

#[tokio::test]
async fn fleet_loaded_posture_without_current_matches_the_full_scheduler() -> Result<()> {
    for (queued, resident_work, resident_scheduling) in [
        (false, true, true),
        (false, true, false),
        (false, false, false),
        (true, false, false),
    ] {
        let (_home, host, runtime) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        let run_loop = runtime.pause_run_loop_for_monitor_test().await;
        runtime.await_initial_resources().await;
        let refresh = runtime.hold_resource_refresh_for_test().await?;
        runtime
            .create_work_item("roster work".into(), None, None, Vec::new())
            .await?;
        if queued {
            runtime
                .enqueue(MessageEnvelope::new(
                    runtime.inner.mutation_owner.agent_id(),
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: None,
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: "roster queued input".into(),
                    },
                ))
                .await?;
        }
        let mut owner = runtime.begin_mutation().await?;
        owner.prepare_current_scheduler_query(None).await?;
        let root = owner.capture_published_root()?;
        drop(owner);
        let (scheduler, identity, metadata) = runtime.roster_scheduler_from_root(&root)?;
        assert_eq!(metadata.has_queued_input, queued);
        let expected = scheduler.scheduling_posture;
        assert_eq!(
            expected.posture,
            if queued {
                crate::types::AgentSchedulingPosture::HasQueuedInput
            } else {
                crate::types::AgentSchedulingPosture::HasRunnableWork
            }
        );
        let mut reservation = runtime.inner.mutation_owner.reserve().await?;
        reservation.begin("roster-without-current".into())?;
        reservation.committed_prepared_with_scheduling_domains(
            root.state.clone(),
            root.episodes.clone(),
            None,
            resident_work.then(|| root.work.clone()).flatten(),
            None,
            if resident_scheduling {
                root.scheduling.clone()
            } else {
                None
            },
        )?;
        let unprepared = reservation.publish(root.state.clone())?;
        drop(reservation);
        assert!(unprepared.current.is_none());
        assert_eq!(unprepared.work.is_some(), resident_work);
        assert_eq!(unprepared.scheduling.is_some(), resident_scheduling);
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE audit_events RENAME TO roster_forbidden_events;
                 ALTER TABLE briefs RENAME TO roster_forbidden_briefs;
                 ALTER TABLE queue_entries RENAME TO roster_forbidden_queue;
                 ALTER TABLE agent_states RENAME TO roster_forbidden_state;",
            )?;
            Ok(())
        })?;
        let entries = host.list_agent_entries().await?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].scheduling_posture, expected);
        assert_eq!(entries[0].identity, identity);
        assert_eq!(entries[0].pending, root.state.pending);
        let published = runtime.inner.mutation_owner.capture()?.unwrap();
        assert!(Arc::ptr_eq(&unprepared, &published));
        runtime.runtime_db().transaction(|tx| {
            tx.execute_batch(
                "ALTER TABLE roster_forbidden_events RENAME TO audit_events;
                 ALTER TABLE roster_forbidden_briefs RENAME TO briefs;
                 ALTER TABLE roster_forbidden_queue RENAME TO queue_entries;
                 ALTER TABLE roster_forbidden_state RENAME TO agent_states;",
            )?;
            Ok(())
        })?;
        drop(refresh);
        drop(run_loop);
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn fleet_queue_posture_tracks_original_writes_and_recovery() -> Result<()> {
    use crate::queue::QueuePostureFacts;
    use crate::runtime_db::transitions::{
        PreparedQueueTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
        TransitionFaultPoint,
    };
    use crate::types::AgentSchedulingPosture;

    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let run_loop = runtime.pause_run_loop_for_monitor_test().await;
    runtime.await_initial_resources().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let owner = &runtime.inner.mutation_owner;
    let db = runtime.runtime_db();
    let id = owner.agent_id();
    let check = |pending, has_queued, has_interrupted| -> Result<_> {
        let root = owner.capture()?.context("missing root")?;
        assert_eq!(
            root.queue_posture,
            Some(QueuePostureFacts {
                pending,
                has_queued,
                has_interrupted,
            })
        );
        assert_eq!(root.state.pending, pending);
        assert_eq!(
            db.agent_states()
                .latest(id)?
                .context("missing State")?
                .pending,
            pending
        );
        assert!(root
            .current
            .as_ref()
            .and_then(|current| current.query.as_ref())
            .is_none());
        assert_eq!(
            db.queue_entries().has_queued_for_agent(id)?,
            has_queued || has_interrupted
        );
        Ok(root.token)
    };
    let prompt = || {
        MessageEnvelope::new(
            id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "roster Queue publication".into(),
            },
        )
    };
    check(0, false, false)?;
    let message = runtime.enqueue(prompt()).await?;
    let message_id = message.id.clone();
    check(1, true, false)?;
    let expected = db
        .queue_entries()
        .latest(&message.id)?
        .context("missing queued source")?;
    let mut interrupted = expected.clone();
    interrupted.status = QueueEntryStatus::Interrupted;
    interrupted.updated_at = runtime.now();
    let mut mutation = runtime.begin_mutation().await?;
    let error = mutation
        .commit_prepared_queue(
            "roster-queue-interrupted".into(),
            PreparedQueueTransition::new(QueueTransitionCommand {
                agent_id: id.to_owned(),
                operation: QueueOperation::Settle,
                mutation: QueueMutation::CompareAndSet {
                    expected,
                    record: interrupted,
                },
                scheduler_claim_work_item: None,
                agent_state: None,
                message_evidence: vec![message],
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: Vec::new(),
                notify_scheduler: false,
                fault: Some(TransitionFaultPoint::BeforeCacheUpdate),
                brief_evidence: Vec::new(),
            }),
        )
        .await
        .expect_err("fixture must lose cache installation after the original commit");
    assert!(error.to_string().contains("reconciliation"), "{error:#}");
    drop(mutation);
    assert!(owner.capture().is_err());
    runtime.reconcile_posture_observation().await?;
    let interrupted_cut = check(1, false, true)?;
    let entries = host.list_agent_entries().await?;
    let entry = &entries[0];
    assert_ne!(
        entry.scheduling_posture.posture,
        AgentSchedulingPosture::HasQueuedInput
    );
    assert_eq!(entry.pending, 1);
    assert_eq!(check(1, false, true)?, interrupted_cut);

    runtime.enqueue(prompt()).await?;
    let mixed_cut = check(2, true, true)?;
    let entries = host.list_agent_entries().await?;
    let entry = &entries[0];
    assert_eq!(
        entry.scheduling_posture.posture,
        AgentSchedulingPosture::HasQueuedInput
    );
    assert_eq!(entry.pending, 2);
    assert_eq!(check(2, true, true)?, mixed_cut);

    let (revision, legacy_pending): (u64, Option<usize>) = db.connection()?.query_row(
        "SELECT control_revision, json_extract(payload_json,'$.pending') FROM agent_states WHERE agent_id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(legacy_pending, None);
    let expected = db
        .queue_entries()
        .latest(&message_id)?
        .context("missing interrupted source")?;
    let mut dropped = expected.clone();
    dropped.status = QueueEntryStatus::Dropped;
    dropped.updated_at = runtime.now();
    let mut mutation = runtime.begin_mutation().await?;
    let state = mutation.committed_state().clone();
    let mut candidate = state.clone();
    candidate.pending -= 1;
    let commit = mutation
        .commit_prepared_queue(
            "roster-queue-drop".into(),
            PreparedQueueTransition::new(QueueTransitionCommand {
                agent_id: id.to_owned(),
                operation: QueueOperation::RepairDrop,
                mutation: QueueMutation::CompareAndSet {
                    expected,
                    record: dropped,
                },
                scheduler_claim_work_item: None,
                agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(state)),
                    record: Box::new(candidate),
                }),
                message_evidence: Vec::new(),
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: Vec::new(),
                notify_scheduler: false,
                fault: None,
                brief_evidence: Vec::new(),
            }),
        )
        .await?;
    assert!(commit.applied);
    drop(mutation);
    assert_ne!(check(1, true, false)?, mixed_cut);
    let after: u64 = db.connection()?.query_row(
        "SELECT control_revision FROM agent_states WHERE agent_id=?1",
        [id],
        |row| row.get(0),
    )?;
    assert_eq!(
        after, revision,
        "a pending-only change rewrote durable State"
    );

    // A pre-upgrade row can carry a stale count. Cold and loaded reads agree.
    db.connection()?.execute(
        "UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending',999) WHERE agent_id=?1",
        [id],
    )?;
    check(1, true, false)?;
    drop(refresh);
    drop(run_loop);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn restart_projects_pending_from_queue_with_a_legacy_state_row() -> Result<()> {
    let mut harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let id = runtime.instance_agent_id();
    runtime
        .enqueue(MessageEnvelope::new(
            id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "queued across restart".into(),
            },
        ))
        .await?;
    assert_eq!(runtime.agent_state().await?.pending, 1);
    runtime.runtime_db().connection()?.execute(
        "UPDATE agent_states SET payload_json=json_set(payload_json,'$.pending',999) WHERE agent_id=?1",
        [id],
    )?;
    harness.restart();
    let runtime = harness.runtime();
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("restart did not publish State")?;
    assert_eq!(root.state.pending, 1);
    assert_eq!(
        root.queue_posture,
        Some(crate::queue::QueuePostureFacts {
            pending: 1,
            has_queued: true,
            has_interrupted: false,
        })
    );
    Ok(())
}

#[tokio::test]
async fn fleet_loaded_entry_uses_one_ready_cut_without_state_lock_body_sql_or_root_retention(
) -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let _run_loop = runtime.pause_run_loop_for_monitor_test().await;
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prepare = runtime.begin_mutation().await?;
    prepare.prepare_current_scheduler_query(None).await?;
    drop(prepare);
    drop(runtime.agent_summary().await?);
    let expected = runtime.agent_list_entry().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(AuditEvent::legacy("roster_cut", serde_json::json!({})))
        .await?;
    assert!(old_root.upgrade().is_none());
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE agent_states RENAME TO fleet_forbidden_states;
             ALTER TABLE agent_identities RENAME TO fleet_forbidden_identity;
             ALTER TABLE queue_entries RENAME TO fleet_forbidden_queue;
             ALTER TABLE audit_events RENAME TO fleet_forbidden_events;
             ALTER TABLE briefs RENAME TO fleet_forbidden_briefs;
             ALTER TABLE tasks RENAME TO fleet_forbidden_tasks;
             ALTER TABLE wait_conditions RENAME TO fleet_forbidden_waits;
             ALTER TABLE timers RENAME TO fleet_forbidden_timers;
             ALTER TABLE timer_wakes RENAME TO fleet_forbidden_wakes;
             ALTER TABLE external_triggers RENAME TO fleet_forbidden_triggers;",
        )?;
        Ok(())
    })?;
    let _agent = runtime.inner.agent.lock().await;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let actual = tokio::time::timeout(std::time::Duration::from_secs(5), host.list_agent_entries())
        .await??;
    assert_eq!(denied.attempts(), 0);
    assert_eq!(actual.len(), 1);
    assert_eq!(*actual[0], expected);
    Ok(())
}

#[tokio::test]
async fn fleet_loaded_entry_ignores_reserved_owner_but_observes_cancellation_and_deadline(
) -> Result<()> {
    use crate::runtime_db::RuntimeDbReadScope;
    use std::time::{Duration, Instant};
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let _run_loop = runtime.pause_run_loop_for_monitor_test().await;
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let _agent = runtime.inner.agent.lock().await;
    tokio::time::timeout(Duration::from_secs(5), host.list_agent_entries()).await??;
    for cancel in [true, false] {
        let deadline = if cancel {
            Instant::now() + Duration::from_secs(30)
        } else {
            Instant::now() - Duration::from_millis(1)
        };
        let scope = RuntimeDbReadScope::new(deadline);
        if cancel {
            scope.cancel();
        }
        let db = runtime.runtime_db().with_read_scope(scope);
        assert!(host
            .list_agent_entries_for_parent_in(None, &db)
            .await
            .is_err());
    }
    drop(owner);
    Ok(())
}

#[tokio::test]
async fn full_summary_ready_cut_avoids_body_sql_agent_lock_and_root_retention() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prepare = runtime.begin_mutation().await?;
    prepare.prepare_current_scheduler_query(None).await?;
    drop(prepare);
    let first = runtime.agent_summary().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(AuditEvent::legacy(
            "summary_cut",
            serde_json::json!({"keeps_original_cut": true}),
        ))
        .await?;
    assert!(old_root.upgrade().is_none());
    let counts = runtime.inner.storage.summary_record_counts()?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE agent_states RENAME TO summary_forbidden_states;
             ALTER TABLE agent_identities RENAME TO summary_forbidden_identity;
             ALTER TABLE queue_entries RENAME TO summary_forbidden_queue;
             ALTER TABLE audit_events RENAME TO summary_forbidden_events;
             ALTER TABLE briefs RENAME TO summary_forbidden_briefs;
             ALTER TABLE tasks RENAME TO summary_forbidden_tasks;
             ALTER TABLE wait_conditions RENAME TO summary_forbidden_waits;
             ALTER TABLE timers RENAME TO summary_forbidden_timers;
             ALTER TABLE timer_wakes RENAME TO summary_forbidden_wakes;
             ALTER TABLE external_triggers RENAME TO summary_forbidden_triggers;",
        )?;
        Ok(())
    })?;
    let _owner = runtime.inner.mutation_owner.reserve().await?;
    let _agent = runtime.inner.agent.lock().await;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let next =
        tokio::time::timeout(std::time::Duration::from_secs(5), runtime.agent_summary()).await??;
    assert_eq!(denied.attempts(), 0);
    drop(denied);
    assert_eq!(next.agent, first.agent);
    assert_eq!(next.identity, first.identity);
    assert_eq!(next.execution, first.execution);
    assert_eq!(next.active_wait_conditions, first.active_wait_conditions);
    assert_eq!((next.recent_brief_count, next.recent_event_count), counts);
    assert!(Arc::ptr_eq(
        &next.skills.discoverable_skills,
        &next.skills.attached_skills,
    ));
    let observation = next
        .material
        ._resources
        .as_ref()
        .unwrap()
        .observation_weak();
    drop(first);
    let phase = next.material.acquisition.as_ref().unwrap().guard()?;
    let weak = Arc::downgrade(&phase);
    drop(phase);
    let expected = serde_json::to_value(&*next)?;
    let bytes = next.into_json_bytes()?;
    runtime.inner.resources.registry.close()?;
    let cache = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?,
        expected
    );
    drop(bytes);
    assert!(weak.upgrade().is_some());
    assert!(observation.upgrade().is_some());
    drop(cache);
    assert!(weak.upgrade().is_some());
    drop(slice);
    assert!(weak.upgrade().is_none());
    assert!(observation.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn full_summary_counts_raw_event_metadata_without_decoding_or_qualifying_body() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    for _ in 0..105 {
        runtime
            .inner
            .storage
            .append_event(&AuditEvent::legacy("raw_count", serde_json::json!({})))?;
    }
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE audit_events SET data_json='corrupt JSON, metadata only' WHERE agent_id=?1",
            [runtime.instance_agent_id()],
        )?;
        Ok(())
    })?;
    let counts = runtime.inner.storage.summary_record_counts()?;
    let summary = runtime.agent_summary().await?;
    assert_eq!(counts.1, 100);
    assert_eq!(
        (summary.recent_brief_count, summary.recent_event_count),
        counts
    );
    let root = runtime.capture_observer_root(runtime.runtime_db())?;
    let query = root.current.as_ref().unwrap().query.as_ref().unwrap();
    assert!(query
        .capture(
            &crate::runtime_db::current_query::CurrentSelection {
                events_after: Some(0),
                ..Default::default()
            },
            &runtime.inner.input_evidence_budget,
        )
        .is_err());
    let mut foreign = root.as_ref().clone();
    foreign.token.identity_incarnation += 1;
    let error = runtime
        .summary_scheduler_from_root(&foreign)
        .err()
        .context("foreign Summary root must fail before projection")?;
    assert!(error.to_string().contains("Summary root scope differs"));
    Ok(())
}

#[tokio::test]
async fn full_summary_wire_one_byte_short_latches_without_changing_prior_bytes() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let scope = super::super::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.instance_agent_id().to_owned(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let mut summary = runtime.agent_summary().await?;
    let value = serde_json::to_value(&*summary)?;
    let size = super::super::input_evidence_budget::serialized_bytes(&*summary)?;
    let holder = u64::try_from(std::mem::size_of::<SummaryWire>() + 128)?;
    let phase = ResourceReadMaterial::with_test_limits(ledger, &scope, 1024 * 1024, 8192)?;
    let header = phase.finish()?.0;
    drop(phase);
    let required = header + (holder + size) * 8;
    summary.material.acquisition = Some(ResourceReadMaterial::with_test_limits(
        ledger, &scope, required, 8192,
    )?);
    let bytes = summary.into_json_bytes()?;
    let mut rejected = runtime.agent_summary().await?;
    let phase = ResourceReadMaterial::with_test_limits(ledger, &scope, required - 1, 8192)?;
    rejected.material.acquisition = Some(phase.clone());
    assert!(rejected.into_json_bytes().is_err());
    assert!(phase.finish().is_err());
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes)?, value);
    Ok(())
}
