//! Actual Task creator, registration, Host target and original binding ACK.
use super::*;
use crate::types::*;
use std::time::Duration;
use tokio::sync::oneshot;
struct Fixture {
    _home: tempfile::TempDir,
    host: crate::host::RuntimeHost,
    runtime: RuntimeHandle,
    child: RuntimeHandle,
    task: TaskRecord,
    source: Arc<OriginalChildTaskProducer>,
    lease: Arc<crate::runtime::control_cancellation::ControlJoinLease>,
    start: oneshot::Sender<()>,
    done: oneshot::Receiver<Result<()>>,
}
async fn fixture() -> Result<Fixture> {
    let (home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let target = host
        .create_named_agent("binding-original-target", None)
        .await?;
    let child = host.get_public_agent(&target.agent_id).await?;
    let task = runtime
        .create_agent_invocation_task(
            "binding before Running".into(),
            "actual original invocation".into(),
            AuthorityClass::OperatorInstruction,
            Some(target.agent_id.clone()),
            false,
            ChildAgentWorkspaceMode::Inherit,
        )
        .await?;
    let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
    let mut detail = task.detail.clone().unwrap();
    detail["target_agent_id"] = serde_json::json!(target.agent_id);
    detail["target_identity_incarnation"] = serde_json::json!(target.incarnation);
    detail["child_turn_baseline"] = serde_json::json!(0);
    source.retain_monitor_stage(
        task.clone(),
        AuthorityClass::OperatorInstruction,
        false,
        false,
        AgentLifecycleDisposition::Retain,
        target.agent_id.clone(),
        0,
        None,
        detail.clone(),
    )?;
    runtime.bind_original_child_monitor_target(&source, &target.agent_id, None, 0)?;
    let (start, started) = oneshot::channel();
    let (send, done) = oneshot::channel();
    let monitor = runtime.clone();
    let original = task.clone();
    let producer = source.clone();
    lease.attach(tokio::spawn(async move {
        if started.await.is_err() {
            return;
        }
        let result = monitor
            .monitor_spawned_child_agent_task(
                original,
                producer,
                AuthorityClass::OperatorInstruction,
                false,
                false,
                AgentLifecycleDisposition::Retain,
                target.agent_id,
                0,
                None,
                detail,
            )
            .await;
        let _ = send.send(result);
    }));
    runtime.inner.task_handles.lock().await.insert(
        task.id.clone(),
        crate::runtime::command_task::ManagedTaskHandle::Async(
            crate::runtime::control_cancellation::SharedControlTaskHandle(lease.clone()),
        ),
    );
    drop(registration);
    Ok(Fixture {
        _home: home,
        host,
        runtime,
        child,
        task,
        source,
        lease,
        start,
        done,
    })
}
fn receipt_count(f: &Fixture) -> Result<usize> {
    Ok(f.runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
        [format!("child-binding:{}", f.lease.lease_id())],
        |row| row.get(0),
    )?)
}
async fn finish(f: Fixture) -> Result<()> {
    f.start.send(()).unwrap();
    f.child
        .enqueue(MessageEnvelope::new(
            f.child.agent_state().await?.id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "complete actual bound child invocation".into(),
            },
        ))
        .await?;
    tokio::time::timeout(Duration::from_secs(15), f.done).await???;
    f.lease.join().await?;
    assert!(f.source.is_durable_finished());
    f.runtime.join_owned_mutations().await?;
    f.host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn original_binding_whole_ack_precedes_any_running_input_and_survives_lost_publication(
) -> Result<()> {
    for lose_ack in [false, true] {
        let f = fixture().await?;
        let (_state, session) = f.runtime.begin_task_birth_session().await?;
        let original = f.runtime.inner.mutation_owner.capture()?.unwrap();
        let message_count:usize=f.runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM messages WHERE kind='task_status' AND json_extract(payload_json,'$.task_id')=?1",[&f.task.id],|row|row.get(0))?;
        assert_eq!(message_count, 0);
        if lose_ack {
            f.source.lose_binding_ack_for_test();
        }
        let first = f
            .runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await;
        assert_eq!(first.is_err(), lose_ack, "{first:?}");
        assert_eq!(receipt_count(&f)?, 1);
        let saved = f.source.binding_descriptor_for_test().await;
        if lose_ack {
            assert!(saved.is_some());
            assert!(f.runtime.inner.mutation_owner.capture().is_err());
            assert!(!f.source.monitor_binding_confirmed());
            assert!(
                f.runtime
                    .continue_original_child_monitor_binding(f.source.clone())
                    .await?
            );
        }
        assert!(f.source.monitor_binding_confirmed());
        assert!(
            f.source.binding_descriptor_for_test().await.is_none(),
            "confirmed capsule releases its owned Task body"
        );
        let root = f.runtime.inner.mutation_owner.capture()?.unwrap();
        assert!(Arc::ptr_eq(&root.state, &original.state));
        assert!(Arc::ptr_eq(
            root.current.as_ref().context("binding lost Current")?,
            original
                .current
                .as_ref()
                .context("Current frontier missing")?,
        ));
        drop(f.runtime.capture_run_once_statuses(&session)?);
        assert!(root
            .work
            .as_ref()
            .zip(original.work.as_ref())
            .is_none_or(|(a, b)| Arc::ptr_eq(a, b)));
        assert!(root
            .input_evidence
            .as_ref()
            .zip(original.input_evidence.as_ref())
            .is_none_or(|(a, b)| Arc::ptr_eq(a, b)));
        let actual = f.runtime.capture_current_task_record(&f.task.id)?;
        let task = actual.record().unwrap();
        assert_eq!(task.status, f.task.status);
        assert_eq!(task.created_at, f.task.created_at);
        assert_eq!(task.recovery, f.task.recovery);
        assert_eq!(task.work_item_id, f.task.work_item_id);
        assert_eq!(
            crate::runtime::tasks::unbound_task_rejoin_generation(task),
            Some(1)
        );
        assert_eq!(task.detail.as_ref().unwrap()["child_turn_baseline"], 0);
        assert_eq!(
            task.detail.as_ref().unwrap()["target_identity_incarnation"],
            1
        );
        assert!(
            f.runtime
                .continue_original_child_monitor_binding(f.source.clone())
                .await?
        );
        assert_eq!(receipt_count(&f)?, 1);
        assert_eq!(f.child.agent_state().await?.turn_index, 0);
        drop(actual);
        drop(root);
        drop(original);
        drop(session);
        finish(f).await?;
    }
    Ok(())
}
#[tokio::test]
async fn original_binding_receipt_rollback_retries_exact_business_descriptor_without_respawn(
) -> Result<()> {
    let f = fixture().await?;
    f.runtime.runtime_db().transaction(|tx| {tx.execute_batch("CREATE TRIGGER binding_receipt_rollback BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce LIKE 'child-binding:%' BEGIN SELECT RAISE(ABORT,'original binding receipt rollback'); END;")?;Ok(())})?;
    assert!(f
        .runtime
        .continue_original_child_monitor_binding(f.source.clone())
        .await
        .is_err());
    let descriptor = f
        .source
        .binding_descriptor_for_test()
        .await
        .context("rollback lost frozen binding")?;
    assert_eq!(receipt_count(&f)?, 0);
    assert!(!f.source.monitor_binding_confirmed());
    assert_eq!(
        f.runtime.runtime_db().tasks().latest(&f.task.id)?.unwrap(),
        f.task
    );
    f.runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("DROP TRIGGER binding_receipt_rollback")?;
        Ok(())
    })?;
    assert!(
        f.runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await?
    );
    let actual: String = f.runtime.runtime_db().connection()?.query_row(
        "SELECT prepared_json FROM input_mutation_receipts WHERE command_nonce=?1",
        [descriptor.command_nonce()],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<crate::runtime_db::input_mutation_receipts::PreparedInputMutation>(
            &actual
        )?,
        descriptor
    );
    assert_eq!(receipt_count(&f)?, 1);
    assert_eq!(f.child.agent_state().await?.turn_index, 0);
    finish(f).await
}
#[tokio::test]
async fn original_binding_no_state_or_retired_owner_does_not_read_successor_or_release_responsibility(
) -> Result<()> {
    let f = fixture().await?;
    let scope = &f.source.address().scope;
    let state: String = f.runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    let identity: String = f.runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [&scope.agent_id],
        |row| row.get(0),
    )?;
    let before = f.runtime.inner.input_evidence_budget.report()?.pending;
    f.runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id=?1",
            [&scope.agent_id],
        )?;
        Ok(())
    })?;
    assert!(
        !f.runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await?
    );
    assert!(f.source.has_terminal_responsibility());
    assert!(!f.source.monitor_binding_confirmed());
    assert_eq!(receipt_count(&f)?, 0);
    f.runtime.runtime_db().transaction(|tx| {
        let mut successor: serde_json::Value = serde_json::from_str(&identity)?;
        successor["incarnation"] = serde_json::json!(scope.identity_incarnation + 1);
        tx.execute(
            "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
            rusqlite::params![serde_json::to_string(&successor)?, scope.agent_id],
        )?;
        crate::runtime_db::evidence::upsert_agent_state_tx(
            tx,
            &serde_json::from_str::<AgentState>(&state)?,
        )?;
        tx.execute(
            "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
            rusqlite::params!["{".repeat(65536), scope.agent_id],
        )?;
        Ok(())
    })?;
    assert!(
        !f.runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await?
    );
    assert!(f.source.monitor_stage_present());
    assert_eq!(receipt_count(&f)?, 0);
    assert_eq!(
        f.runtime.inner.input_evidence_budget.report()?.pending,
        before
    );
    f.runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
            rusqlite::params![identity, scope.agent_id],
        )?;
        tx.execute(
            "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
            rusqlite::params![state, scope.agent_id],
        )?;
        Ok(())
    })?;
    assert!(
        f.runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await?
    );
    finish(f).await
}
#[tokio::test]
async fn original_binding_target_raw_budget_precedes_decode_and_rechecks_target_incarnation(
) -> Result<()> {
    let f = fixture().await?;
    let target = f.source.original_monitor_binding_target()?;
    let id = target.target_id();
    let raw: String = f.runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let mut request = TaskDomainLoadRequest {
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024,
    };
    let baseline = f
        .runtime
        .inner
        .input_evidence_budget
        .report()?
        .observed_bytes;
    f.runtime.runtime_db().connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        rusqlite::params!["{".repeat(1024 * 1024), id],
    )?;
    let error = f
        .runtime
        .runtime_db()
        .transaction(|tx| {
            PreparedInputTransition::prepare_original_child_monitor_binding_tx(
                tx,
                &f.source,
                f.runtime.now(),
                &request,
                &f.runtime.inner.input_evidence_budget,
            )
        })
        .err()
        .context("bad oversized target decoded")?;
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
            Some(crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        f.runtime
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes,
        baseline
    );
    let mut changed: serde_json::Value = serde_json::from_str(&raw)?;
    changed["incarnation"] = serde_json::json!(2);
    f.runtime.runtime_db().connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        rusqlite::params![serde_json::to_string(&changed)?, id],
    )?;
    request.byte_limit = 64 * 1024 * 1024;
    assert!(f
        .runtime
        .runtime_db()
        .transaction(
            |tx| PreparedInputTransition::prepare_original_child_monitor_binding_tx(
                tx,
                &f.source,
                f.runtime.now(),
                &request,
                &f.runtime.inner.input_evidence_budget
            )
        )
        .is_err());
    assert_eq!(receipt_count(&f)?, 0);
    assert!(!f.source.monitor_binding_confirmed());
    f.runtime.runtime_db().connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        rusqlite::params![raw, id],
    )?;
    assert!(
        f.runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await?
    );
    finish(f).await
}

#[tokio::test]
async fn binding_lost_ack_recovery_prepares_only_current_live_roles() -> Result<()> {
    let f = fixture().await?;
    let lease = f
        .runtime
        .register_task_query(crate::runtime::task_queries::TaskQuerySpec {
            output_task_ids: &[f.task.id.as_str()],
            ..Default::default()
        })?;
    f.source.lose_binding_ack_for_test();
    let first = f
        .runtime
        .continue_original_child_monitor_binding(f.source.clone())
        .await;
    assert!(first.is_err(), "{first:?}");
    assert_eq!(receipt_count(&f)?, 1);
    assert!(f.runtime.inner.mutation_owner.capture().is_err());
    let original_descriptor = f.source.binding_descriptor_for_test().await.unwrap();
    let coverage = || -> Result<crate::runtime::resident_task::TaskCoverage> {
        let raw: String = f.runtime.runtime_db().connection()?.query_row(
            "SELECT coverage_json FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            rusqlite::params![f.task.agent_id, f.source.address().scope.identity_incarnation],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&raw)?)
    };
    let before = coverage()?;
    assert!(before
        .required_keys
        .contains(&crate::runtime::resident_task::TaskKey::new(
            crate::runtime::resident_task::TaskFamily::Task,
            &f.task.id,
        )));
    drop(lease);
    assert!(
        f.runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await?
    );
    let root = f.runtime.inner.mutation_owner.capture()?.unwrap();
    let after = coverage()?;
    assert_eq!(
        after.revision, before.revision,
        "selector preparation is not a business write"
    );
    assert!(after.required_keys.is_empty());
    assert!(root
        .tasks
        .as_ref()
        .unwrap()
        .coverage()
        .required_keys
        .is_empty());
    assert_eq!(receipt_count(&f)?, 1);
    let stored: String = f.runtime.runtime_db().connection()?.query_row(
        "SELECT command_digest FROM input_mutation_receipts WHERE command_nonce=?1",
        [format!("child-binding:{}", f.lease.lease_id())],
        |row| row.get(0),
    )?;
    assert_eq!(stored, original_descriptor.command_digest());
    assert!(f.source.binding_descriptor_for_test().await.is_none());
    assert!(f.source.monitor_binding_confirmed());
    drop(root);
    finish(f).await?;
    Ok(())
}

#[tokio::test]
async fn binding_queue_container_and_source_share_one_clamped_phase_allowance() -> Result<()> {
    use crate::runtime_db::task_snapshot::TaskDomainLoadRequest;
    let f = fixture().await?;
    let dispatch_gate = Arc::new(tokio::sync::Semaphore::new(0));
    f.runtime
        .set_clock_recovery_dispatch_gate_for_test(dispatch_gate.clone());
    for index in 0..32 {
        f.runtime
            .enqueue(
                MessageEnvelope::new(
                    f.task.agent_id.clone(),
                    MessageKind::InternalFollowup,
                    MessageOrigin::System {
                        subsystem: "binding-budget-test".into(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Background,
                    MessageBody::Text {
                        text: format!("binding held queue {index}"),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::RuntimeSystem,
                    AdmissionContext::RuntimeOwned,
                ),
            )
            .await?;
    }
    let ledger = &f.runtime.inner.input_evidence_budget;
    let full = TaskDomainLoadRequest {
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        query: Default::default(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    let at = f.source.original_monitor_binding_clock(f.runtime.now())?;
    let estimate = f.runtime.runtime_db().transaction(|tx| {
        let prepared = PreparedInputTransition::prepare_original_child_monitor_binding_tx(
            tx, &f.source, at, &full, ledger,
        )?
        .unwrap();
        Ok(prepared
            .original_child_monitor_binding()
            .unwrap()
            .material_estimate())
    })?;
    let reservations = f
        .host
        .reserve_mutation_scopes(&[(
            f.task.agent_id.clone(),
            f.source.address().scope.identity_incarnation,
        )])
        .await?;
    let root = reservations[0].capture_ready_snapshot()?.unwrap();
    let before = ledger.report()?.observed_bytes;
    let mut remaining = full.clone();
    let facts = f
        .runtime
        .prepare_original_child_binding_facts(
            &reservations[0],
            root.state.clone(),
            root.episodes.clone(),
            root.input_evidence.clone(),
            &mut remaining,
        )
        .await?;
    let queue = facts.original_child_binding_queue_material()?;
    assert!(queue.records > 4, "actual queue must be nonempty");
    assert_eq!(full.byte_limit - remaining.byte_limit, queue.bytes);
    assert_eq!(full.record_limit - remaining.record_limit, queue.records);
    let weak = facts.task_stop_queue_metadata_test_weak().unwrap();
    assert!(weak.upgrade().is_some());
    drop(facts);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, before);
    drop(root);
    drop(reservations);
    let small = TaskDomainLoadRequest {
        byte_limit: estimate.bytes.checked_add(queue.bytes).unwrap() - 1,
        ..full.clone()
    };
    assert!(estimate.bytes < small.byte_limit && queue.bytes < small.byte_limit);
    // Source alone fits this exact cap. No business SQL has run yet.
    f.runtime.runtime_db().transaction(|tx| {
        assert!(
            PreparedInputTransition::prepare_original_child_monitor_binding_tx(
                tx, &f.source, at, &small, ledger
            )?
            .is_some()
        );
        Ok(())
    })?;
    let stored: String = f.runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM tasks WHERE task_id=?1",
        [&f.task.id],
        |r| r.get(0),
    )?;
    let pending = ledger.report()?.pending;
    let mut original = None;
    let error = f
        .runtime
        .inner
        .host_bridge
        .as_ref()
        .unwrap()
        .commit_original_child_monitor_binding(&f.source, &mut original, &small)
        .await
        .err()
        .context("combined binding material unexpectedly fit")?;
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
            Some(crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(receipt_count(&f)?, 0);
    assert_eq!(ledger.report()?.pending, pending);
    assert_eq!(
        f.runtime.runtime_db().connection()?.query_row(
            "SELECT payload_json FROM tasks WHERE task_id=?1",
            [&f.task.id],
            |r| r.get::<_, String>(0)
        )?,
        stored
    );
    assert!(f.source.monitor_stage_present() && !f.source.monitor_binding_confirmed());
    assert!(
        f.runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await?
    );
    dispatch_gate.add_permits(1024);
    finish(f).await?;
    Ok(())
}

#[tokio::test]
async fn accepted_binding_updates_only_existing_metadata_under_parent_cleanup_and_whole_ack(
) -> Result<()> {
    for lost_publication in [false, true] {
        let f = fixture().await?;
        let identity = f
            .runtime
            .runtime_db()
            .agent_identities()
            .latest(f.runtime.instance_agent_id())?
            .context("original identity missing")?;
        // Begin the real deletion metadata, then exercise the exact ParentCleanup
        // fence mode as a DB actor fixture. This does not grant its admission or
        // claim that a live active Task passes the idle ParentCleanup selector.
        let (_, mut job, _) = f.runtime.runtime_db().agent_deletions().begin(
            &identity.agent_id,
            identity.revision,
            "test:original-binding-fence",
            false,
        )?;
        let operator_job = serde_json::to_string(&job)?;
        job.mode = AgentDeletionMode::ParentCleanup;
        f.runtime.runtime_db().transaction(|tx| {
            tx.execute(
                "UPDATE agent_deletion_jobs SET payload_json=?2 WHERE deletion_id=?1",
                rusqlite::params![job.deletion_id, serde_json::to_string(&job)?],
            )?;
            Ok(())
        })?;
        let mut new_work = f.task.clone();
        new_work.status = TaskStatus::Running;
        new_work.updated_at = f.task.updated_at + chrono::Duration::milliseconds(1);
        let error = f
            .runtime
            .runtime_db()
            .tasks()
            .upsert(&new_work)
            .expect_err("ordinary active work must retain the ParentCleanup fence");
        assert!(format!("{error:#}").contains("agent_cleanup_fenced"));
        let inputs: usize = f.runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
            [&f.task.agent_id],
            |r| r.get(0),
        )?;
        f.runtime.runtime_db().transaction(|tx| {tx.execute_batch("CREATE TRIGGER parent_cleanup_binding_receipt_fail BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce LIKE 'child-binding:%' BEGIN SELECT RAISE(ABORT,'original ParentCleanup binding receipt rollback'); END;")?;Ok(())})?;
        assert!(f
            .runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await
            .is_err());
        assert_eq!(receipt_count(&f)?, 0);
        assert_eq!(
            f.runtime.runtime_db().tasks().latest(&f.task.id)?.unwrap(),
            f.task
        );
        assert!(f.source.has_terminal_responsibility());
        f.runtime.runtime_db().transaction(|tx| {
            tx.execute_batch("DROP TRIGGER parent_cleanup_binding_receipt_fail")?;
            Ok(())
        })?;
        if lost_publication {
            f.source.lose_binding_ack_for_test();
        }
        let result = f
            .runtime
            .continue_original_child_monitor_binding(f.source.clone())
            .await;
        assert_eq!(result.is_err(), lost_publication, "{result:?}");
        if lost_publication {
            assert!(f.runtime.inner.mutation_owner.capture().is_err());
            assert!(
                f.runtime
                    .continue_original_child_monitor_binding(f.source.clone())
                    .await?
            );
        }
        assert!(f.source.monitor_binding_confirmed());
        assert_eq!(receipt_count(&f)?, 1);
        let stored = f.runtime.runtime_db().tasks().latest(&f.task.id)?.unwrap();
        assert_eq!(stored.status, f.task.status);
        assert_eq!(stored.created_at, f.task.created_at);
        assert_eq!(stored.recovery, f.task.recovery);
        assert_eq!(stored.work_item_id, f.task.work_item_id);
        assert_eq!(
            stored.detail.as_ref().unwrap()["target_identity_incarnation"],
            1
        );
        let after: usize = f.runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
            [&f.task.agent_id],
            |r| r.get(0),
        )?;
        assert_eq!(inputs, after, "binding cannot enqueue across cleanup fence");
        // Restore the real operator mode solely for the original terminal test
        // continuation. The tested binding remains the exact committed receipt.
        f.runtime.runtime_db().transaction(|tx| {
            tx.execute(
                "UPDATE agent_deletion_jobs SET payload_json=?2 WHERE deletion_id=?1",
                rusqlite::params![job.deletion_id, operator_job],
            )?;
            Ok(())
        })?;
        finish(f).await?;
    }
    Ok(())
}

#[tokio::test]
async fn actual_new_subagent_invocation_has_exact_durable_recovery_and_typed_target_unavailability(
) -> Result<()> {
    use crate::runtime_db::child_task_terminal::{
        observe_original_child_monitor_recovery_handoff_tx,
        OriginalChildMonitorRecoveryDisposition as Disposition,
    };
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let task = runtime
        .create_agent_invocation_task(
            "actual new child recovery".into(),
            "accepted new child prompt".into(),
            AuthorityClass::OperatorInstruction,
            None,
            true,
            ChildAgentWorkspaceMode::Inherit,
        )
        .await?;
    let model = runtime.model_state_for(&runtime.agent_state().await?);
    let spawned = runtime
        .inner
        .host_bridge
        .as_ref()
        .unwrap()
        .spawn_child_task(
            runtime.clone(),
            &task,
            "accepted new child prompt".into(),
            AuthorityClass::OperatorInstruction,
            false,
            None,
            AgentModelResolution {
                requested: None,
                resolved_provider: model.effective_model.provider.as_str().into(),
                resolved_model: model.effective_model.model,
                resolved_parameters: None,
                resolution_status: AgentModelResolutionStatus::Inherited,
                policy_notes: vec![],
            },
        )
        .await?;
    let mut bound = task.clone();
    bound.updated_at = task.updated_at + chrono::Duration::milliseconds(1);
    bound.detail = Some(spawned.task_detail.clone());
    // Preserve exactly the real creator's original complete/partial contract.
    bound.detail =
        Some(runtime.task_detail_preserving_rejoin_contract(&task, bound.detail.take().unwrap()));
    runtime
        .persist_task_transition(&bound, "actual_new_child_recovery_bound")
        .await?;
    let (registration, lease, source) = runtime.admit_original_child_monitor(&bound).await?;
    source.retain_monitor_stage(
        bound.clone(),
        AuthorityClass::OperatorInstruction,
        false,
        false,
        AgentLifecycleDisposition::Retain,
        spawned.child_agent_id.clone(),
        spawned.child_turn_baseline,
        spawned.delivery_id.clone(),
        spawned.task_detail.clone(),
    )?;
    runtime.bind_original_child_monitor_target(
        &source,
        &spawned.child_agent_id,
        spawned.delivery_id.as_deref(),
        spawned.child_turn_baseline,
    )?;
    let (_send, wait) = oneshot::channel::<()>();
    lease.attach(tokio::spawn(async move {
        let _ = wait.await;
    }));
    runtime.inner.task_handles.lock().await.insert(
        bound.id.clone(),
        crate::runtime::command_task::ManagedTaskHandle::Async(
            crate::runtime::control_cancellation::SharedControlTaskHandle(lease.clone()),
        ),
    );
    drop(registration);
    lease.cancel();
    lease.join().await?;
    let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    };
    let held = runtime.inner.mutation_owner.reserve().await?;
    let proof = runtime
        .runtime_db()
        .transaction(|tx| {
            observe_original_child_monitor_recovery_handoff_tx(
                tx,
                &source,
                &request,
                &runtime.inner.input_evidence_budget,
            )
        })?
        .context("created_new_subagent true cannot strand its real recovery")?;
    assert_eq!(proof.disposition(), Disposition::RecheckExistingTarget);
    runtime.runtime_db().transaction(|tx| {
        let raw:String=tx.query_row("SELECT payload_json FROM agent_identities WHERE agent_id=?1",[&spawned.child_agent_id],|row|row.get(0))?;
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&spawned.child_agent_id])?;
        let error=observe_original_child_monitor_recovery_handoff_tx(tx,&source,&request,&runtime.inner.input_evidence_budget).err().context("successor incarnation cannot replace original recovery target")?;
        assert!(format!("{error:#}").contains("original incarnation/association"));
        tx.execute("UPDATE agent_identities SET payload_json=?2,status='active' WHERE agent_id=?1",rusqlite::params![spawned.child_agent_id,raw])?;
        // Explicit repository-state unavailability fixture: not child exit/FS
        // evidence, and no successor State is consulted or granted.
        tx.execute("UPDATE agent_identities SET status='deleted',payload_json=json_set(payload_json,'$.status','deleted') WHERE agent_id=?1",[&spawned.child_agent_id])?;
        let unavailable=observe_original_child_monitor_recovery_handoff_tx(tx,&source,&request,&runtime.inner.input_evidence_budget)?.context("unavailable target must retain explicit bootstrap Interrupted responsibility")?;
        assert_eq!(unavailable.disposition(),Disposition::InterruptedUnavailableTarget);
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.status','active'),status='active' WHERE agent_id=?1",[&spawned.child_agent_id])?;
        Ok(())
    })?;
    assert!(source.observed_terminal().is_none());
    assert!(source.frozen_terminal()?.is_none());
    source.acknowledge_monitor_recovery_handoff(&proof)?;
    drop(held);
    assert!(source.is_durable_finished());
    assert!(source.monitor_stage().is_none());
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&bound.id)?
            .unwrap()
            .recovery,
        bound.recovery
    );
    // The same real Host normal retirement/reload now takes the canonical Task
    // recovery path. No Stop or old-monitor child outcome is manufactured.
    host.unload_runtime(runtime.instance_agent_id()).await;
    let current = host.default_runtime().await?;
    assert!(current.instance_owner_epoch() > runtime.instance_owner_epoch());
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if current
                .runtime_db()
                .tasks()
                .latest(&bound.id)?
                .is_some_and(|task| task.status == TaskStatus::Completed)
            {
                break Ok::<(), anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    let stops: usize = current.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM task_stop_effects WHERE task_id=?1",
        [&bound.id],
        |row| row.get(0),
    )?;
    assert_eq!(stops, 0);
    host.shutdown().await?;
    Ok(())
}
