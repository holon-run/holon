use super::*;
use crate::{runtime::tests::support::LifecycleHarness, types::*};

fn message(id: &str) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text: id.into() },
    );
    message.id = id.into();
    message.created_at = chrono::DateTime::from_timestamp(1_000, 0).unwrap();
    message
}

#[tokio::test]
async fn run_once_poll_waits_for_original_writer_but_never_repairs_unknown() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE messages RENAME TO messages_poll_forbidden;
             ALTER TABLE tasks RENAME TO tasks_poll_forbidden;
             ALTER TABLE agent_states RENAME TO states_poll_forbidden;",
        )?;
        Ok(())
    })?;
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    reservation.begin("run-once-original-writer".into())?;
    assert!(runtime.capture_run_once_statuses(&session).is_err());
    {
        let poll = runtime.capture_run_once_statuses_after_writer(&session);
        tokio::pin!(poll);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut poll)
                .await
                .is_err()
        );
        reservation.definitely_not_committed()?;
        drop(reservation);
        let read = poll.await?;
        assert!(Arc::ptr_eq(&read.state, &root.state));
    }
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    reservation.begin("run-once-real-unknown".into())?;
    drop(reservation);
    assert!(runtime
        .capture_run_once_statuses_after_writer(&session)
        .await
        .is_err());
    assert!(runtime.inner.mutation_owner.capture().is_err());
    Ok(())
}

#[tokio::test]
async fn run_once_event_report_retains_original_body_without_an_owned_body_copy() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    runtime
        .append_original_audit_event(AuditEvent::legacy(
            "run_once_borrowed_event",
            serde_json::json!({"body": "x".repeat(4096)}),
        ))
        .await?;
    let read = runtime.capture_run_once(&session)?;
    assert_eq!(read.events().count(), 1);
    let original = std::ptr::from_ref(read.events().next().unwrap());
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let (tasks, messages, holder) = runtime.run_once_records(read)?;
    assert!(tasks.is_empty() && messages.is_empty());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes - before,
        128 * 4,
        "only the original fixed product metadata is owned"
    );
    assert_eq!(
        std::ptr::from_ref(holder.events().next().unwrap()),
        original
    );
    let copies = Arc::downgrade(&holder._copies);
    drop(holder);
    assert!(copies.upgrade().is_none());
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes <= before);
    Ok(())
}

#[tokio::test]
async fn run_once_all_born_heads_and_old_reply_union_exceed_the_terminal_window() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let reply = runtime.enqueue(message("old-shared-run-reply")).await?;
    let prior = status_task("native-task");
    runtime
        .persist_task_transition(&prior, "task_queued")
        .await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let count = crate::runtime::resident_task::TASK_TERMINAL_WINDOW + 1;
    for index in (0..count).rev() {
        let mut born = status_task(&format!("born-output-{index:04}"));
        born.kind = TaskKind::AgentMessageWait;
        born.status = TaskStatus::Completed;
        born.detail = Some(serde_json::json!({"message_id": &reply.id}));
        runtime
            .persist_task_transition(&born, "task_completed")
            .await?;
    }
    runtime.persist_tool_execution_evidence(&tool()).await?;
    runtime
        .append_original_audit_event(AuditEvent::legacy(
            "process_execution_requested",
            serde_json::json!({
                "surface": "command_task",
                "promoted_from_exec_command": true,
                "task_id": &prior.id,
            }),
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    let poll = runtime.capture_run_once_statuses(&session)?;
    assert!(Arc::ptr_eq(&poll.state, &root.state));
    assert_eq!(poll.task_ids()?.count(), count);
    assert_eq!(
        poll.referenced_task_ids().collect::<Vec<_>>(),
        [prior.id.as_str(), prior.id.as_str()]
    );
    assert_eq!(poll.task_output_status(&prior.id)?, TaskStatus::Queued);
    for id in poll.task_ids()? {
        assert_eq!(poll.task_output_status(id)?, TaskStatus::Completed);
        poll.tasks.output_source(id)?;
    }
    assert!(poll.messages().all(|message| message.id != reply.id));
    let (read, prepared) = runtime.prepare_run_once_task_outputs(&session).await?;
    assert!(Arc::ptr_eq(&read.state, &root.state));
    assert_eq!(read.task_ids()?.count(), count);
    drop((root, poll, read));
    runtime
        .enqueue(message("after-prepared-run-output"))
        .await?;
    assert!(old_root.upgrade().is_none());
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO all_born_tasks_forbidden;
             ALTER TABLE messages RENAME TO all_born_messages_forbidden;
             ALTER TABLE agent_states RENAME TO all_born_state_forbidden;
             ALTER TABLE agent_identities RENAME TO all_born_identity_forbidden;",
        )?;
        Ok(())
    })?;
    let output = runtime.render_run_once_task_outputs(prepared).await?;
    assert_eq!(output.snapshots.len(), count);
    for (index, snapshot) in output.snapshots.iter().enumerate() {
        assert_eq!(snapshot.task_id, format!("born-output-{index:04}"));
        assert_eq!(snapshot.status, TaskStatus::Completed);
        assert_eq!(snapshot.output_preview, "old-shared-run-reply");
    }
    Ok(())
}

fn tool() -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: "native-tool".into(),
        agent_id: "default".into(),
        work_item_id: None,
        turn_index: 0,
        turn_id: None,
        tool_name: "ExecCommand".into(),
        created_at: chrono::Utc::now(),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::OperatorInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({"cmd":"printf test"}),
        output: serde_json::json!({"result":{"task_handle":{"task_id":"native-task"}}}),
        summary: "command promoted".into(),
        invocation_surface: None,
    }
}

fn status_task(id: &str) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Queued,
        created_at: chrono::DateTime::from_timestamp(1_000, 0).unwrap(),
        updated_at: chrono::DateTime::from_timestamp(1_000, 0).unwrap(),
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail: None,
        recovery: None,
    }
}

#[tokio::test]
async fn poll_task_status_union_keeps_one_cut_and_releases_the_old_root() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prior = status_task("native-task");
    runtime
        .persist_task_transition(&prior, "task_queued")
        .await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let mut born = status_task("born-task");
    runtime
        .persist_task_transition(&born, "task_queued")
        .await?;
    runtime.persist_tool_execution_evidence(&tool()).await?;
    runtime
        .append_original_audit_event(AuditEvent::legacy(
            "process_execution_requested",
            serde_json::json!({
                "surface": "command_task",
                "promoted_from_exec_command": true,
                "task_id": prior.id,
            }),
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    let old = runtime.capture_run_once_statuses(&session)?;
    assert!(Arc::ptr_eq(&old.state, &root.state));
    assert_eq!(old.task_ids()?.collect::<Vec<_>>(), ["born-task"]);
    assert_eq!(
        old.referenced_task_ids().collect::<Vec<_>>(),
        ["native-task", "native-task"]
    );
    assert_eq!(old.task_output_status(&prior.id)?, TaskStatus::Queued);
    assert_eq!(old.task_output_status(&born.id)?, TaskStatus::Queued);
    prior.status = TaskStatus::Cancelled;
    born.status = TaskStatus::Completed;
    runtime
        .persist_task_transition(&prior, "task_cancelled")
        .await?;
    runtime
        .persist_task_transition(&born, "task_completed")
        .await?;
    drop(root);
    assert!(old_root.upgrade().is_none());
    let retained = Arc::downgrade(
        old.tasks
            .records
            .iter()
            .find(|record| record.locator.key.id == prior.id)
            .unwrap(),
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO tasks_poll_status_forbidden;
             ALTER TABLE messages RENAME TO messages_poll_status_forbidden;
             ALTER TABLE audit_events RENAME TO events_poll_status_forbidden;
             ALTER TABLE agent_states RENAME TO states_poll_status_forbidden;
             ALTER TABLE agent_identities RENAME TO identities_poll_status_forbidden;",
        )?;
        Ok(())
    })?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let current = runtime.capture_run_once_statuses(&session)?;
    for _ in 0..16 {
        assert_eq!(old.task_output_status(&prior.id)?, TaskStatus::Queued);
        assert_eq!(old.task_output_status(&born.id)?, TaskStatus::Queued);
        assert_eq!(
            current.task_output_status(&prior.id)?,
            TaskStatus::Cancelled
        );
        assert_eq!(current.task_output_status(&born.id)?, TaskStatus::Completed);
    }
    assert!(retained.upgrade().is_some());
    drop(current);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    drop(old);
    assert!(retained.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn poll_task_reference_does_not_authorize_an_unprepared_task() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, session) = runtime.begin_task_birth_session().await?;
    runtime.persist_tool_execution_evidence(&tool()).await?;
    assert_eq!(runtime.capture_run_once(&session)?.tools().count(), 1);
    let error = runtime.capture_run_once_statuses(&session).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::NeedsPreparation(key))
                if key == &TaskKey::new(TaskFamily::Task, "native-task")
        ),
        "{error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn combined_birth_capture_uses_one_cut_without_history_sql_or_root_retention() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.enqueue(message("before-session")).await?;
    let (_, session) = runtime.begin_task_birth_session().await?;
    runtime.enqueue(message("z-first")).await?;
    runtime.persist_tool_execution_evidence(&tool()).await?;
    let old_root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_weak = Arc::downgrade(&old_root);
    let old_read = runtime.capture_run_once(&session)?;
    assert!(Arc::ptr_eq(&old_read.state, &old_root.state));
    assert_eq!(
        old_read
            .messages()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["z-first"]
    );
    assert_eq!(
        old_read.tools().next().unwrap().command.task_id.as_deref(),
        Some("native-task")
    );
    let (_, later) = runtime.begin_task_birth_session().await?;
    runtime.enqueue(message("a-second")).await?;
    drop(old_root);
    assert!(old_weak.upgrade().is_none());
    assert_eq!(old_read.messages().count(), 1);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE messages RENAME TO messages_run_once_forbidden;
             ALTER TABLE tool_executions RENAME TO tools_run_once_forbidden;
             ALTER TABLE tasks RENAME TO tasks_run_once_forbidden;
             ALTER TABLE agent_states RENAME TO states_run_once_forbidden;
             ALTER TABLE agent_identities RENAME TO identities_run_once_forbidden;",
        )?;
        Ok(())
    })?;
    let current = runtime.capture_run_once(&session)?;
    assert_eq!(current.inputs.query.born.len(), 3);
    let (_, messages, holder) = runtime.run_once_records(current)?;
    assert_eq!(
        messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["z-first", "a-second"]
    );
    let late_read = runtime.capture_run_once(&later)?;
    assert_eq!(
        late_read
            .messages()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["a-second"]
    );
    assert_eq!(late_read.tools().count(), 0);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop((messages, holder, late_read));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn repeated_pure_polls_do_not_spend_or_reset_the_original_source_phase() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, session) = runtime.begin_task_birth_session().await?;
    runtime.enqueue(message("born-message")).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let source = root
        .input_evidence
        .as_ref()
        .unwrap()
        .original_query_source()?
        .unwrap();
    let original = source.acquisition().finish()?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    for _ in 0..64 {
        let read = runtime.capture_run_once(&session)?;
        assert_eq!(read.messages().count(), 1);
        assert!(read.task_ids()?.next().is_none());
        drop(read);
        assert_eq!(source.acquisition().finish()?, original);
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    Ok(())
}

#[test]
fn message_order_matches_sql_null_sequence_time_and_id() {
    let mut messages = [
        ("late-sequence", Some(8), 1),
        ("null-z", None, 2),
        ("same-sequence-z", Some(3), 2),
        ("null-a", None, 2),
        ("same-sequence-a", Some(3), 2),
        ("early-time", Some(3), 1),
    ]
    .map(|(id, sequence, time)| {
        let mut record = message(id);
        record.message_seq = sequence;
        record.created_at = chrono::DateTime::from_timestamp(time, 0).unwrap();
        record
    });
    messages.sort_by(compare_message_order);
    assert_eq!(
        messages.map(|record| record.id),
        [
            "null-a",
            "null-z",
            "early-time",
            "same-sequence-a",
            "same-sequence-z",
            "late-sequence"
        ]
    );
}

#[tokio::test]
async fn final_tool_loan_is_exact_bounded_and_retained_until_product_release() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let (_, session) = runtime.begin_task_birth_session().await?;
    let mut original = tool();
    original.output["full_product_only"] = serde_json::json!("x".repeat(4096));
    runtime.persist_tool_execution_evidence(&original).await?;
    // Initial completion does not fence later refresh notifications. Keep
    // unrelated resource acquisitions out of this exact loan accounting cut.
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let read = runtime.capture_run_once(&session)?;
    let before_report = runtime.inner.input_evidence_budget.report()?;
    let before = before_report.observed_bytes;
    let (tools, material) = runtime.run_once_tools(&read)?;
    let loan_guard = Arc::downgrade(&material.guard()?);
    assert_eq!(
        serde_json::to_value(&tools[0])?,
        serde_json::to_value(&original)?
    );
    let (_, _, mut holder) = runtime.run_once_records(read)?;
    holder.retain_tools(material);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > before);
    let retained = runtime.inner.input_evidence_budget.report()?;
    drop(tools);
    let released = runtime.inner.input_evidence_budget.report()?;
    assert!(loan_guard.upgrade().is_some());
    assert_eq!(
        released.observed_bytes, retained.observed_bytes,
        "retained: {retained:#?}\nreleased: {released:#?}"
    );
    drop(holder);
    assert!(loan_guard.upgrade().is_none());
    let after = runtime.inner.input_evidence_budget.report()?;
    assert!(
        after.observed_bytes < before,
        "before: {before_report:#?}\nretained: {retained:#?}\nreleased: {released:#?}\nafter: {after:#?}"
    );
    Ok(())
}

#[tokio::test]
async fn final_tool_loan_refuses_foreign_ledger_changed_body_and_missing_alias() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, session) = runtime.begin_task_birth_session().await?;
    let original = tool();
    runtime.persist_tool_execution_evidence(&original).await?;
    let read = runtime.capture_run_once(&session)?;
    let foreign = LifecycleHarness::new();
    assert!(foreign.runtime().run_once_tools(&read).is_err());
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE tool_executions SET payload_json=json_set(payload_json,'$.duration_ms',42)
             WHERE evidence_id=?1",
            [&original.id],
        )?;
        Ok(())
    })?;
    let error = runtime
        .run_once_tools(&read)
        .err()
        .context("changed Tool body accepted")?;
    assert!(error.to_string().contains("digest changed"), "{error:#}");
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "DELETE FROM input_evidence_current WHERE record_id=?1 AND family='tool_execution'",
            [&original.id],
        )?;
        Ok(())
    })?;
    let error = runtime
        .run_once_tools(&read)
        .err()
        .context("missing Tool alias accepted")?;
    assert!(error.to_string().contains("unavailable"), "{error:#}");
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    Ok(())
}
