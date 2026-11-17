//! Freeze an ordinary attachment from a real, canonically admitted live turn.
use super::*;
use crate::runtime_db::transitions::{LiveInputAttachmentCommand, PreparedInputTransition};
use crate::types::{LiveInputBatchRecord, LiveInputSafePoint};

async fn fresh_attachment(
    runtime: &RuntimeHandle,
) -> Result<(PreparedInputTransition, Arc<ResidentTaskDomain>)> {
    let receiver = owner(runtime, "frozen ordinary attachment").await;
    let unrelated = TaskRecord {
        id: "attachment-oracle-unrelated-task".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: runtime.now(),
        updated_at: runtime.now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("unrelated terminal Task".into()),
        detail: Some(serde_json::json!({ "body": "unrelated".repeat(128) })),
        recovery: None,
    };
    runtime
        .persist_task_transition(&unrelated, "task_completed")
        .await?;
    message(
        runtime,
        Some(&receiver),
        "attachment-oracle-peer",
        "anchor",
        "anchor",
        true,
    )
    .await;
    let scheduled = Box::pin(claim(runtime)).await;
    Box::pin(
        runtime.begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        ),
    )
    .await?;
    runtime
        .record_incoming_transcript_entry(&scheduled.message)
        .await?;
    let source = message(
        runtime,
        Some(&receiver),
        "attachment-oracle-peer",
        "ordinary",
        "ordinary input for the open canonical turn",
        true,
    )
    .await;

    let mutation = runtime.begin_mutation().await?;
    let scope = mutation.input_evidence_scope();
    let material = crate::diagnostics::resource_io::ResourceReadMaterial::new(
        &runtime.inner.input_evidence_budget,
        &scope,
    )?;
    let (expected_state, _, cancelled) = mutation.idle_selection_snapshot(&material)?;
    assert!(!cancelled);
    let command = runtime.with_idle_read(Some(&expected_state), &material, |read| {
        let binding = expected_state
            .current_execution_binding
            .as_ref()
            .context("canonical live binding")?;
        let activation = binding
            .activation_id
            .as_deref()
            .context("canonical activation")?;
        let execution = read.protocol()?.context("execution protocol")?;
        let attempt = execution
            .attempts
            .get(activation)
            .context("open live attempt")?;
        let turn = read
            .shared_turn(&binding.turn_id)?
            .context("open live turn")?;
        assert!(turn.terminal.is_none());
        assert!(turn.live_input_batches.is_empty());
        let queue = read.queue(&source.id)?.context("queued ordinary input")?;
        assert_eq!(queue.status, QueueEntryStatus::Queued);
        let cutoff = runtime.now();
        let item = crate::runtime::input_consumption::projection::qualified_item_after(
            read,
            &source,
            128_000,
            24 * 1024 - LiveInputBatchRecord::HEADER.len(),
            &format!(
                "[current_live_input_batches]\n{}",
                LiveInputBatchRecord::HEADER
            ),
        )?
        .context("qualified ordinary item")?;
        let mut record = (*expected_state).clone();
        record.pending = record.pending.saturating_sub(1);
        Ok(LiveInputAttachmentCommand {
            expected_attempt: attempt.clone(),
            expected_turn: (*turn).clone(),
            batch: LiveInputBatchRecord {
                activation_id: activation.into(),
                safe_point: LiveInputSafePoint::new(activation, 2),
                cutoff,
                max_items: 8,
                max_bytes: 24 * 1024,
                rendered_bytes: LiveInputBatchRecord::HEADER.len() + item.rendered.len(),
                result_identities: Vec::new(),
                ordinary_message_ids: vec![source.id.clone()],
                items: vec![item],
            },
            messages: vec![source.clone()],
            queues: vec![QueueEntryRecord {
                status: QueueEntryStatus::Interjected,
                updated_at: cutoff,
                ..(*queue).clone()
            }],
            agent_state: AgentStateMutation {
                expected: Some(Box::new((*expected_state).clone())),
                record: Box::new(record),
            },
            audit_event_id: crate::ids::audit_event_id(),
            fault: None,
        })
    })?;
    let resident = mutation
        .capture_published_root()?
        .tasks
        .clone()
        .context("published nonempty Task domain")?;
    assert_eq!(
        resident
            .coverage()
            .observations
            .get(&TaskKey::new(TaskFamily::Task, &unrelated.id)),
        Some(&TaskAvailability::Present),
    );
    assert!(resident
        .selected_install_records()
        .iter()
        .any(|record| record.fact == CanonicalTaskFact::Task(unrelated.clone())));
    let prepared = PreparedInputTransition::live_input_attachment(
        scope,
        crate::ids::audit_event_id(),
        runtime.now(),
        command,
    )?;
    Ok((prepared, resident))
}

#[tokio::test]
async fn p2_fresh_ordinary_attachment_matches_full_task_oracle() -> Result<()> {
    let (_data, _workspace, runtime, _provider) = fixture().await;
    let (prepared, _resident) = fresh_attachment(&runtime).await?;
    super::live_input_task_oracle::compare_prepared_live_command(&runtime, prepared).await
}
