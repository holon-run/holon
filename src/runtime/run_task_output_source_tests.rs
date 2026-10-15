use super::*;
use crate::runtime::tests::support::LifecycleHarness;
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
async fn prepare_named(runtime: &RuntimeHandle) -> Result<()> {
    let mut held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("fixture Root missing")?;
    let scope = root.tasks.as_ref().context("fixture Task missing")?.scope();
    let roles = held.input_query_roles(scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    let events = held.current_event_roles(scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    let (inputs, current) =
        crate::runtime_db::input_evidence_snapshot::prepare_named_run_sources_for_fixture(
            runtime.runtime_db(),
            scope,
            &runtime.inner.input_evidence_budget,
            roles.as_ref(),
            held.owner_epoch(),
            &root.state,
            events.as_ref(),
        )?;
    held.begin("actual-run-task-output-fixture".into())?;
    held.committed_prepared_with_current_read_domains(
        root.state.clone(),
        root.episodes.clone(),
        Some(Arc::new(inputs)),
        root.work.clone(),
        root.tasks.clone(),
        root.scheduling.clone(),
        Some(current),
    )?;
    held.publish(root.state.clone())?;
    Ok(())
}
fn task(runtime: &RuntimeHandle, id: &str) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        agent_id: runtime.instance_agent_id().into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Queued,
        created_at: runtime.now(),
        updated_at: runtime.now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some(id.into()),
        detail: None,
        recovery: None,
    }
}
#[tokio::test]
async fn original_run_task_output_uses_one_cut_and_keeps_source_through_result_without_sql(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    prepare_named(runtime).await?;
    let sessions = runtime.begin_run_birth_sessions().await?;
    let task = task(runtime, "born-output-one-cut");
    let mut message = MessageEnvelope::new(
        runtime.instance_agent_id(),
        MessageKind::TaskStatus,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "exact original output".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.task_id = Some(task.id.clone());
    message.metadata = Some(
        serde_json::json!({"task_id":task.id,"task_kind":"command_task","task_status":"completed"}),
    );
    runtime
        .persist_task_transition_with_message(&task, "task_queued", &message)
        .await?;
    prepare_named(runtime).await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("Root missing")?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let report = Arc::new(
        runtime
            .capture_run_birth_sessions_from_snapshot(&sessions, &root, LIMITS)?
            .project_report(LIMITS)?,
    );
    let weak = Arc::downgrade(&report);
    let prior = report.material_estimate()?;
    let strong = Arc::strong_count(&root);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE tasks RENAME TO run_output_forbidden_tasks;
        ALTER TABLE messages RENAME TO run_output_forbidden_messages;
        ALTER TABLE agent_states RENAME TO run_output_forbidden_states;
        ALTER TABLE audit_events RENAME TO run_output_forbidden_events;",
    )?;
    let other = LifecycleHarness::new();
    other
        .runtime()
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let other_before = other
        .runtime()
        .inner
        .input_evidence_budget
        .report()?
        .observed_bytes;
    let original_before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let foreign = runtime.capture_run_task_output_from_snapshot(
        &root,
        report.clone(),
        &task.id,
        LIMITS,
        prior,
    )?;
    let material = foreign.material_estimate();
    let error = other
        .runtime()
        .render_run_task_output(foreign, material)
        .await
        .err()
        .context("foreign Runtime rendered original selected Task")?;
    assert!(
        format!("{error:#}").contains("original owner or actual ledger"),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        original_before
    );
    assert_eq!(
        other
            .runtime()
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes,
        other_before
    );
    let selection = runtime.capture_run_task_output_from_snapshot(
        &root,
        report.clone(),
        &task.id,
        LIMITS,
        prior,
    )?;
    assert_eq!(Arc::strong_count(&root), strong);
    assert_eq!(selection.read.status()?, TaskStatus::Completed);
    assert_eq!(
        render_task_message_body(&selection.read.source()?.unwrap().body),
        "exact original output"
    );
    let material = selection.material_estimate();
    drop(report);
    let result = runtime.render_run_task_output(selection, material).await?;
    assert_eq!(result.result.task.status, TaskStatus::Completed);
    assert_eq!(result.result.task.output_preview, "");
    assert_eq!(
        result.result.task.result_summary.as_deref(),
        Some("exact original output")
    );
    assert!(result.material_estimate().0 > material.0);
    assert!(weak.upgrade().is_some());
    assert_eq!(Arc::strong_count(&root), strong);
    drop(result);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
#[tokio::test]
async fn original_run_wait_reply_before_birth_is_selected_explicitly_and_native_render_keys_are_bounded(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let reply = runtime
        .enqueue(MessageEnvelope::new(
            runtime.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Next,
            MessageBody::Json {
                value: serde_json::Value::Object(
                    (0..512)
                        .map(|n| {
                            (
                                format!("original-old-reply-key-{n}"),
                                serde_json::Value::Null,
                            )
                        })
                        .collect(),
                ),
            },
        ))
        .await?;
    prepare_named(runtime).await?;
    let sessions = runtime.begin_run_birth_sessions().await?;
    let mut task = task(runtime, "born-wait-original-old-reply");
    task.kind = TaskKind::AgentMessageWait;
    task.status = TaskStatus::Completed;
    task.detail = Some(serde_json::json!({"message_id":reply.id}));
    runtime
        .persist_task_transition(&task, "task_completed")
        .await?;
    let mut other_task = task.clone();
    other_task.id = "born-wait-other-recall-target".into();
    runtime
        .persist_task_transition(&other_task, "task_completed")
        .await?;
    prepare_named(runtime).await?;
    let original_reply = runtime
        .storage()
        .read_message_by_id(&reply.id)?
        .context("original canonical old reply missing")?;
    assert!(
        reply.message_seq.is_none(),
        "enqueue returns the original pre-assignment envelope"
    );
    assert!(
        original_reply.message_seq.is_some(),
        "the original repository assigns native Message sequence"
    );
    let mut expected_reply = reply.clone();
    expected_reply.message_seq = original_reply.message_seq;
    assert_eq!(
        original_reply, expected_reply,
        "all original fields other than assigned sequence remain exact"
    );
    let role = runtime.register_task_query(TaskQuerySpec {
        output_task_ids: &[&task.id, &other_task.id],
        ..Default::default()
    })?;
    {
        let mut owner = runtime.begin_mutation().await?;
        owner.prepare_owner_task_query(&role).await?;
        owner.prepare_task_output_inputs(&role, &task.id).await?;
    }
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("Root missing")?;
    let report = Arc::new(
        runtime
            .capture_run_birth_sessions_from_snapshot(&sessions, &root, LIMITS)?
            .project_report(LIMITS)?,
    );
    assert!(!report
        .born_inputs()
        .any(|row| row.locator.key.family == InputEvidenceFamily::Message
            && row.locator.key.id == reply.id));
    let prior = report.material_estimate()?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE tasks RENAME TO run_wait_forbidden_tasks;
        ALTER TABLE messages RENAME TO run_wait_forbidden_messages;",
    )?;
    // The old body is genuinely resident, but its read-only recall belongs to
    // the first exact Task facet. A second born Task cannot inherit that grant.
    let error = runtime
        .capture_run_task_output_from_snapshot(&root, report.clone(), &other_task.id, LIMITS, prior)
        .err()
        .context("other Task inherited original recall qualification")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceReadError>(),
            Some(InputEvidenceReadError::NeedsPreparation(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let mut selection = runtime.capture_run_task_output_from_snapshot(
        &root,
        report.clone(),
        &task.id,
        LIMITS,
        prior,
    )?;
    assert_eq!(selection.read.message(&reply.id)?, &original_reply);
    let material = selection.material_estimate();
    selection.read.material.record_limit = material.1 + 8;
    let error = runtime
        .render_run_task_output(selection, material)
        .await
        .err()
        .context("native render keys ignored")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskReadError>(),
            Some(TaskReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let selection = runtime.capture_run_task_output_from_snapshot(
        &root,
        report.clone(),
        &task.id,
        LIMITS,
        prior,
    )?;
    let material = selection.material_estimate();
    let result = runtime.render_run_task_output(selection, material).await?;
    assert_eq!(result.result.task.status, TaskStatus::Completed);
    assert!(result
        .result
        .task
        .output_preview
        .contains("original-old-reply-key-0"));
    assert!(result.material_estimate().1 > material.1 + 512 * 8);
    drop(result);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
