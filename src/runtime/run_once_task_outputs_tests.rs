use super::*;
use crate::runtime::tests::support::LifecycleHarness;

fn task(id: &str) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::AgentMessageWait,
        status: TaskStatus::Completed,
        created_at: chrono::DateTime::from_timestamp(1_000, 0).unwrap(),
        updated_at: chrono::DateTime::from_timestamp(1_000, 0).unwrap(),
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail: None,
        recovery: None,
    }
}

fn reply() -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "one original shared reply".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::HttpControlPrompt,
        AdmissionContext::ControlAuthenticated,
    )
}

#[tokio::test]
async fn run_once_batch_shared_reply_is_one_original_body_and_one_frozen_cut() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let reply = runtime.enqueue(reply()).await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let mut first = task("z-first-batch-output");
    first.created_at = chrono::DateTime::from_timestamp(999, 0).unwrap();
    first.detail = Some(serde_json::json!({"message_id":reply.id}));
    let mut second = task("a-second-batch-output");
    second.detail = first.detail.clone();
    runtime
        .persist_task_transition(&first, "task_completed")
        .await?;
    runtime
        .persist_task_transition(&second, "task_completed")
        .await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let original_root = runtime.inner.mutation_owner.capture()?.unwrap();
    let root_weak = Arc::downgrade(&original_root);
    let (read, prepared) = runtime.prepare_run_once_task_outputs(&session).await?;
    assert!(Arc::ptr_eq(&read.state, &original_root.state));
    let key = InputEvidenceKey::new(InputEvidenceFamily::Message, &reply.id);
    let body = prepared.reads[0]
        .inputs
        .as_ref()
        .unwrap()
        .lookup(&key)?
        .unwrap();
    assert!(Arc::ptr_eq(
        body,
        prepared.reads[1]
            .inputs
            .as_ref()
            .unwrap()
            .lookup(&key)?
            .unwrap()
    ));
    let retained_body = Arc::downgrade(body);
    assert!(
        read.messages().all(|message| message.id != reply.id),
        "old reply is not birth membership"
    );
    second.summary = Some("later summary not in the captured output".into());
    tokio::time::timeout(
        Duration::from_secs(5),
        runtime.persist_task_transition(&second, "task_updated"),
    )
    .await??;
    drop(original_root);
    assert!(
        root_weak.upgrade().is_none(),
        "prepared products must not retain Root or owner"
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO batch_output_tasks_forbidden;
             ALTER TABLE messages RENAME TO batch_output_messages_forbidden;
             ALTER TABLE agent_states RENAME TO batch_output_state_forbidden;
             ALTER TABLE agent_identities RENAME TO batch_output_identity_forbidden;",
        )?;
        Ok(())
    })?;
    let output = runtime.render_run_once_task_outputs(prepared).await?;
    assert_eq!(
        output
            .snapshots
            .iter()
            .map(|s| s.task_id.as_str())
            .collect::<Vec<_>>(),
        [first.id.as_str(), second.id.as_str()]
    );
    assert!(output
        .snapshots
        .iter()
        .all(|s| s.output_preview == "one original shared reply" && s.summary.is_none()));
    let wire = serde_json::to_string(&output.snapshots)?;
    assert!(!wire.contains("later summary"));
    drop(read);
    assert!(retained_body.upgrade().is_some());
    let last = output.retention.clone();
    drop(output);
    assert!(retained_body.upgrade().is_some());
    drop(last);
    assert!(
        retained_body.upgrade().is_none(),
        "the last product holder releases original reply"
    );
    Ok(())
}

#[tokio::test]
async fn run_once_batch_rejects_changed_reply_without_publishing_an_alias() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let reply = runtime.enqueue(reply()).await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let mut row = task("changed-batch-reply");
    row.detail = Some(serde_json::json!({"message_id":reply.id}));
    runtime
        .persist_task_transition(&row, "task_completed")
        .await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    runtime.runtime_db().transaction(|tx| {
        tx.execute("UPDATE messages SET payload_json=json_set(payload_json,'$.body.text','tampered') WHERE evidence_id=?1",
            [&reply.id])?;
        Ok(())
    })?;
    let result = runtime.prepare_run_once_task_outputs(&session).await;
    assert!(
        result.is_err(),
        "Task metadata cannot bless changed canonical Input"
    );
    assert!(
        Arc::ptr_eq(&root, &runtime.inner.mutation_owner.capture()?.unwrap()),
        "readonly acquisition failure must not replace Root"
    );
    Ok(())
}

#[tokio::test]
async fn run_once_batch_render_rejects_another_runtime_with_the_same_identity() -> Result<()> {
    let first = LifecycleHarness::new();
    let runtime = first.runtime();
    let (_, session) = runtime.begin_task_birth_session().await?;
    runtime
        .persist_task_transition(&task("foreign-batch-render"), "task_completed")
        .await?;
    let (_, prepared) = runtime.prepare_run_once_task_outputs(&session).await?;
    let second = LifecycleHarness::new();
    let result = second
        .runtime()
        .render_run_once_task_outputs(prepared)
        .await;
    assert!(
        result.is_err(),
        "same scoped ID cannot transfer actual ledger material"
    );
    Ok(())
}

#[tokio::test]
async fn run_once_batch_file_and_render_allowance_is_cumulative_one_record_short() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, session) = runtime.begin_task_birth_session().await?;
    let home = tempfile::tempdir()?;
    for id in ["first-batch-file", "second-batch-file"] {
        let path = home.path().join(id);
        std::fs::write(&path, b"original output")?;
        let mut row = task(id);
        row.kind = TaskKind::CommandTask;
        row.detail = Some(serde_json::json!({"output_path":path}));
        runtime
            .persist_task_transition(&row, "task_completed")
            .await?;
    }
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, mut prepared) = runtime.prepare_run_once_task_outputs(&session).await?;
    let render_records = prepared
        .reads
        .iter()
        .map(|read| (read.inputs.as_ref().map_or(0, |i| i.records.len()) + 1) * 8)
        .sum::<usize>();
    let exact = prepared.material.records + render_records + 16;
    assert!(
        exact <= prepared.material.record_limit,
        "fixture fits the original budget"
    );
    prepared.material.record_limit = exact - 1;
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let result = runtime.render_run_once_task_outputs(prepared).await;
    let error = result
        .err()
        .context("second file must not get a replenished allowance")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes < before,
        "failed products release the original SQL, selector, rendering and file guards"
    );
    Ok(())
}
