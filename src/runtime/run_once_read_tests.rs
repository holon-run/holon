use super::*;
use crate::{
    runtime::{
        resident_input_evidence::{InputCaptureError, InputEvidenceFamily},
        resident_task::TaskReadError,
        tests::support::LifecycleHarness,
    },
    types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        TaskKind, TaskRecord,
    },
};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
async fn prepare_named(runtime: &RuntimeHandle) -> Result<()> {
    let mut held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Run Root absent")?;
    let scope = root
        .tasks
        .as_ref()
        .context("actual Task scope absent")?
        .scope();
    let roles = held.input_query_roles(scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    let events = held.current_event_roles(scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    let mut windows = Vec::new();
    crate::runtime::scheduler::include_scheduler_brief_window(&mut windows);
    let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows,
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: LIMITS.0,
        record_limit: LIMITS.1,
    };
    let (inputs, current) = crate::runtime_db::input_evidence_snapshot::prepare_named_run_sources_with_request_for_fixture(
            runtime.runtime_db(),
            scope,
            &runtime.inner.input_evidence_budget,
            roles.as_ref(),
            held.owner_epoch(),
            &root.state,
            events.as_ref(),
            &request,
        )?;
    held.begin("actual-run-read-named-preparation".into())?;
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
fn deny(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE tasks RENAME TO run_read_forbidden_tasks;
         ALTER TABLE messages RENAME TO run_read_forbidden_messages;
         ALTER TABLE agent_states RENAME TO run_read_forbidden_states;
         ALTER TABLE audit_events RENAME TO run_read_forbidden_events;
         ALTER TABLE work_items RENAME TO run_read_forbidden_work;
         ALTER TABLE wait_conditions RENAME TO run_read_forbidden_waits;
         ALTER TABLE queue_entries RENAME TO run_read_forbidden_queue;
         ALTER TABLE timers RENAME TO run_read_forbidden_timers;
         ALTER TABLE external_triggers RENAME TO run_read_forbidden_triggers;
         ALTER TABLE tool_executions RENAME TO run_read_forbidden_tools;
         ALTER TABLE transcript_entries RENAME TO run_read_forbidden_transcripts;",
    )?;
    Ok(())
}
#[tokio::test]
async fn original_run_poll_and_final_select_one_publication_before_render_and_keep_every_source_until_last_holder(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let old_task = task(runtime, "older-promoted-command");
    runtime
        .persist_task_transition(&old_task, "task_queued")
        .await?;
    let mut old_terminal = task(runtime, "older-promoted-terminal-command");
    old_terminal.status = TaskStatus::Completed;
    runtime
        .persist_task_transition(&old_terminal, "task_completed")
        .await?;
    prepare_named(runtime).await?;
    let sessions = runtime.begin_run_birth_sessions().await?;
    let born = task(runtime, "original-one-cut-command");
    let mut message = MessageEnvelope::new(
        runtime.instance_agent_id(),
        MessageKind::TaskStatus,
        MessageOrigin::Task {
            task_id: born.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "original one-cut accepted output".into(),
        },
    );
    message.task_id = Some(born.id.clone());
    message.metadata = Some(
        serde_json::json!({"task_id":born.id,"task_kind":"command_task","task_status":"completed"}),
    );
    message = message.with_admission(
        crate::types::MessageDeliverySurface::TaskRejoin,
        crate::types::AdmissionContext::RuntimeOwned,
    );
    runtime
        .persist_task_transition_with_message(&born, "task_queued", &message)
        .await?;
    runtime
        .append_owned_runtime_audit_event(AuditEvent::legacy(
            "process_execution_requested",
            serde_json::json!({
                "surface":"command_task","promoted_from_exec_command":true,"task_id":old_task.id,
            }),
        ))
        .await?;
    runtime.append_owned_runtime_audit_event(AuditEvent::legacy("process_execution_requested",serde_json::json!({
        "surface":"command_task","promoted_from_exec_command":true,"task_id":old_terminal.id,
    }))).await?;
    prepare_named(runtime).await?;
    let expected_closure = runtime.current_closure().await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Root absent")?;
    let strong = Arc::strong_count(&root);
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    deny(runtime)?;
    let read = runtime.capture_run_poll_read_from_snapshot(&sessions, &root, None, LIMITS)?;
    assert_eq!(read.poll.runtime_error, false);
    assert_eq!(read.closure, expected_closure);
    assert_eq!(read.task_status(&born.id)?, TaskStatus::Completed);
    assert_eq!(read.task_status(&old_task.id)?, TaskStatus::Queued);
    assert_eq!(read.task_status(&old_terminal.id)?, TaskStatus::Completed);
    let (extra, extra_material) = read
        .extra_tasks
        .as_ref()
        .context("older terminal Task was not selected explicitly")?;
    assert!(selected_task(&read.report.source.tasks, &old_terminal.id).is_none());
    assert!(selected_task(&read.scheduler.tasks, &old_terminal.id).is_none());
    assert!(selected_task(extra, &old_terminal.id).is_some());
    extra_material.validate_accounting(&read.report.source.inputs.input.scope, ledger)?;
    assert_eq!(read.active_task_ids, [old_task.id.clone()]);
    assert_eq!(
        read.report
            .born_tasks()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>(),
        [born.id.as_str()]
    );
    assert!(read
        .report
        .born_inputs()
        .any(|r| r.locator.key.family == InputEvidenceFamily::Message
            && r.locator.key.id == message.id));
    assert_eq!(Arc::strong_count(&root), strong);
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let other = LifecycleHarness::new();
    other
        .runtime()
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let other_baseline = other
        .runtime()
        .inner
        .input_evidence_budget
        .report()?
        .observed_bytes;
    let foreign = runtime.capture_run_final_read_from_snapshot(&sessions, &root, None, LIMITS)?;
    let error = foreign
        .render(other.runtime())
        .await
        .err()
        .context("foreign Runtime rendered original final read")?;
    assert!(
        format!("{error:#}").contains("original owner or actual ledger"),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert_eq!(
        other
            .runtime()
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes,
        other_baseline
    );
    let selected = runtime.capture_run_final_read_from_snapshot(&sessions, &root, None, LIMITS)?;
    assert_eq!(selected.outputs.len(), 1);
    assert_eq!(Arc::strong_count(&root), strong);
    let weak = Arc::downgrade(&selected._containers);
    let selected_material = selected.material;
    let rendered = selected.render(runtime).await?;
    assert_eq!(
        rendered.outputs[0].result.task.status,
        TaskStatus::Completed
    );
    assert_eq!(
        rendered.outputs[0].result.task.result_summary.as_deref(),
        Some("original one-cut accepted output")
    );
    assert!(rendered.material_estimate().0 > selected_material.0);
    assert_eq!(Arc::strong_count(&root), strong);
    let original_text = rendered.outputs[0]
        .result
        .task
        .result_summary
        .as_ref()
        .unwrap()
        .as_ptr();
    let before_handoff = rendered.material_estimate();
    let moved = rendered.into_response_tasks(runtime)?;
    assert_eq!(
        moved.tasks[0].result_summary.as_ref().unwrap().as_ptr(),
        original_text,
        "the original rendered Task body is moved without another String copy"
    );
    assert!(moved.source.material_estimate().0 > before_handoff.0);
    assert_eq!(moved.source.poll.active_task_ids, [old_task.id.clone()]);
    assert_eq!(Arc::strong_count(&root), strong);
    let final_read = Arc::new(moved);
    let last = final_read.clone();
    drop(final_read);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn original_run_joint_read_preserves_unprepared_foreign_and_real_remaining_material_errors(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    // A real unsupported evidence ACK invalidates the genuine bootstrap
    // cursor. This exercises the missing Source gate without editing a DTO.
    let entry=crate::types::TranscriptEntry::new(runtime.instance_agent_id(),
        crate::types::TranscriptEntryKind::AssistantRound,Some(0),None,
        serde_json::json!({"text":"actual ACK establishes incomplete Input"}));
    runtime.persist_transcript_evidence(&entry).await?;
    // The initial failure is the original genuine Source gate; no fake Ready.
    let initial = runtime
        .begin_run_birth_sessions()
        .await
        .err()
        .context("unprepared Input was promoted to Ready")?;
    assert!(
        matches!(
            initial.downcast_ref::<InputQueryError>(),
            Some(InputQueryError::Incomplete)
        ),
        "{initial:#}"
    );
    prepare_named(runtime).await?;
    let sessions = runtime.begin_run_birth_sessions().await?;
    let born = task(runtime, "actual-joint-cap-command");
    runtime
        .persist_task_transition(&born, "task_queued")
        .await?;
    prepare_named(runtime).await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Root absent")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let before = ledger.report()?.observed_bytes;
    let read = runtime.capture_run_poll_read_from_snapshot(&sessions, &root, None, LIMITS)?;
    let material = read.material_estimate();
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, before);
    deny(runtime)?;
    let fits = |cap| -> Result<bool> {
        let fits = match runtime.capture_run_poll_read_from_snapshot(&sessions, &root, None, cap) {
            Ok(read) => {
                drop(read);
                true
            }
            Err(error) => {
                assert!(matches!(error.downcast_ref::<InputQueryError>(),Some(InputQueryError::ResourceExhausted(_)))
                    || matches!(error.downcast_ref::<TaskReadError>(),Some(TaskReadError::ResourceExhausted(_)))
                    || matches!(error.downcast_ref::<InputCaptureError>(),Some(InputCaptureError::ResourceExhausted))
                    || matches!(error.downcast_ref::<crate::runtime::current_read_facts::CurrentReadError>(),Some(crate::runtime::current_read_facts::CurrentReadError::ResourceExhausted))
                    || matches!(error.downcast_ref::<crate::runtime::resident_work::WorkReadError>(),Some(crate::runtime::resident_work::WorkReadError::ResourceExhausted(_)))
                    || matches!(error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),Some(crate::runtime::resident_scheduling::SchedulingReadError::ResourceExhausted(_))),"{error:#}");
                false
            }
        };
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(fits)
    };
    assert!(material.0 > 0 && material.1 > 0);
    // Probe each real first-copy peak with the other axis fixed. Request caps
    // are themselves encoded metadata, so different axes are not separable.
    for native in [false, true] {
        let mut bounds = (1, if native { LIMITS.1 } else { LIMITS.0 });
        while bounds.0 < bounds.1 {
            let middle = bounds.0 + (bounds.1 - bounds.0) / 2;
            let cap = if native {
                (LIMITS.0, middle)
            } else {
                (middle, LIMITS.1)
            };
            if fits(cap)? {
                bounds.1 = middle;
            } else {
                bounds.0 = middle + 1;
            }
        }
        let exact = if native {
            (LIMITS.0, bounds.0)
        } else {
            (bounds.0, LIMITS.1)
        };
        let short = if native {
            (LIMITS.0, bounds.0 - 1)
        } else {
            (bounds.0 - 1, LIMITS.1)
        };
        assert!(!fits(short)?);
        assert!(fits(exact)?);
    }
    let other = LifecycleHarness::new();
    let other_before = other
        .runtime()
        .inner
        .input_evidence_budget
        .report()?
        .observed_bytes;
    assert!(other
        .runtime()
        .capture_run_poll_read_from_snapshot(&sessions, &root, None, LIMITS)
        .is_err());
    assert_eq!(
        other
            .runtime()
            .inner
            .input_evidence_budget
            .report()?
            .observed_bytes,
        other_before
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    Ok(())
}
