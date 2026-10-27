use super::*;
use crate::runtime::{mutation_owner::MutationOwnerError, tests::support::LifecycleHarness};
use crate::runtime_db::transitions::{TaskTransitionCommand, TransitionFaultPoint};

fn task(id: &str) -> TaskRecord {
    let at = Utc::now();
    TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: crate::types::TaskKind::CommandTask,
        status: TaskStatus::Queued,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("actual publication fixture".into()),
        detail: Some(serde_json::json!({"output_summary":"accepted output"})),
        recovery: None,
    }
}
fn command(
    task: TaskRecord,
    fault: Option<TransitionFaultPoint>,
    expected_owner_incarnation: u64,
) -> TaskTransitionCommand {
    TaskTransitionCommand {
        agent_id: task.agent_id.clone(),
        expected_owner_incarnation,
        task,
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
        fault,
    }
}
fn forbid_bodies(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO publication_tasks_forbidden;
            ALTER TABLE agent_states RENAME TO publication_states_forbidden;
            ALTER TABLE agent_identities RENAME TO publication_identities_forbidden",
        )?;
        Ok(())
    })
}
fn restore_bodies(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE publication_identities_forbidden RENAME TO agent_identities;
            ALTER TABLE publication_states_forbidden RENAME TO agent_states;
            ALTER TABLE publication_tasks_forbidden RENAME TO tasks",
        )?;
        Ok(())
    })
}

#[tokio::test]
async fn public_task_reads_wait_for_actual_inflight_publication_without_body_sql() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let mut original = task("actual-inflight-publication");
    runtime
        .persist_task_transition(&original, "task_queued")
        .await?;
    drop(runtime.task_output_read(&original.id, false, 0).await?);
    drop(runtime.task_status_read(&original.id).await?);
    drop(runtime.active_task_records_read(50).await?);
    let mut owner = runtime.begin_mutation().await?;
    original.status = TaskStatus::Running;
    original.updated_at = Utc::now();
    let expected = original.clone();
    let operation = crate::ids::audit_event_id();
    // The genuine W1 SQL/whole receipt can finish; its original in-memory
    // installation must await this existing Agent lock before publication.
    let agent = runtime.inner.agent.lock().await;
    let expected_owner_incarnation = runtime.instance_incarnation();
    let writer = tokio::spawn(async move {
        owner
            .commit_task_transition(
                operation,
                command(original, None, expected_owner_incarnation),
                Default::default(),
                false,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if runtime
                .runtime_db()
                .tasks()
                .latest(&expected.id)?
                .is_some_and(|task| task.status == TaskStatus::Running)
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    assert!(!writer.is_finished());
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(matches!(
        runtime.inner.mutation_owner.try_reserve(),
        Err(MutationOwnerError::Busy)
    ));
    forbid_bodies(runtime)?;

    let mut readers = Vec::new();
    for kind in 0..3 {
        let read_runtime = runtime.clone();
        let task_id = expected.id.clone();
        let started = Arc::new(tokio::sync::Notify::new());
        let read_started = started.clone();
        readers.push(tokio::spawn(async move {
            read_started.notify_one();
            match kind {
                0 => {
                    let read = read_runtime.task_output_read(&task_id, false, 0).await?;
                    assert_eq!(read.result.task.status, TaskStatus::Running);
                    assert_eq!(read.result.task.output_preview, "accepted output");
                }
                1 => assert_eq!(
                    read_runtime
                        .task_status_read(&task_id)
                        .await?
                        .snapshot
                        .status,
                    TaskStatus::Running
                ),
                _ => {
                    let read = read_runtime.active_task_records_read(50).await?;
                    assert_eq!(read.records.len(), 1);
                    assert_eq!(read.records[0].status, TaskStatus::Running);
                }
            }
            Ok::<_, anyhow::Error>(())
        }));
        started.notified().await;
        tokio::task::yield_now().await;
        assert!(
            !readers.last().unwrap().is_finished(),
            "ordinary reader must wait for the active writer, not fail its transient gate"
        );
    }
    drop(agent);
    let (commit, deletion) = tokio::time::timeout(Duration::from_secs(10), writer).await???;
    assert!(commit.applied);
    assert!(deletion.is_none());
    for reader in readers {
        tokio::time::timeout(Duration::from_secs(10), reader).await???;
    }
    assert!(runtime.inner.mutation_owner.capture()?.is_some());
    restore_bodies(runtime)?;
    assert_eq!(
        runtime.runtime_db().tasks().latest(&expected.id)?.as_ref(),
        Some(&expected)
    );
    Ok(())
}

#[tokio::test]
async fn abandoned_original_publication_remains_gated_for_every_public_task_read() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let mut original = task("actual-abandoned-publication");
    runtime
        .persist_task_transition(&original, "task_queued")
        .await?;
    drop(runtime.task_output_read(&original.id, false, 0).await?);
    let mut owner = runtime.begin_mutation().await?;
    original.status = TaskStatus::Running;
    original.updated_at = Utc::now();
    let operation = crate::ids::audit_event_id();
    let error = owner
        .commit_task_transition(
            operation.clone(),
            command(
                original.clone(),
                Some(TransitionFaultPoint::BeforeCacheUpdate),
                runtime.instance_incarnation(),
            ),
            Default::default(),
            false,
        )
        .await
        .err()
        .context("actual publication fault was not observed")?;
    assert!(
        error.to_string().contains("committed but publication"),
        "{error:#}"
    );
    drop(owner);
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let before = reservation
        .recovery()?
        .context("original known recovery disappeared")?;
    assert_eq!(before.key.operation_id, operation);
    assert!(before.known.is_some());
    drop(reservation);
    forbid_bodies(runtime)?;
    for kind in 0..3 {
        let error = tokio::time::timeout(Duration::from_secs(10), async {
            match kind {
                0 => runtime.task_output_read(&original.id, false, 0).await.err(),
                1 => runtime.task_status_read(&original.id).await.err(),
                _ => runtime.active_task_records_read(50).await.err(),
            }
        })
        .await?
        .context("abandoned publication was silently recovered or bypassed")?;
        assert!(
            matches!(error.downcast_ref::<MutationOwnerError>(),
            Some(MutationOwnerError::NeedsReconcile(key)) if key.operation_id == operation),
            "{error:#}"
        );
    }
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let after = reservation
        .recovery()?
        .context("ordinary read cleared original recovery")?;
    assert_eq!(after.key, before.key);
    assert_eq!(after.phase, before.phase);
    assert!(after.known.is_some());
    drop(reservation);
    restore_bodies(runtime)?;
    assert_eq!(
        runtime.runtime_db().tasks().latest(&original.id)?.as_ref(),
        Some(&original)
    );
    let receipts: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
        [&operation],
        |row| row.get(0),
    )?;
    assert_eq!(receipts, 1);
    // Only the original explicit mutation recovery actor may open this gate.
    drop(runtime.begin_mutation().await?);
    assert_eq!(
        runtime
            .task_status_read(&original.id)
            .await?
            .snapshot
            .status,
        TaskStatus::Running
    );
    Ok(())
}
