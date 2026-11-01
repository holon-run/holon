use super::*;
use crate::{
    runtime::{
        scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor},
        tests::support::{context_config, tempdir, StubProvider},
    },
    types::{
        AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, ToolExecutionRecord,
        ToolExecutionStatus, TurnTerminalKind, TurnTerminalRecord,
    },
};

async fn started() -> Result<(RuntimeHandle, tempfile::TempDir, TurnTerminalRecord)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new(
        "terminal-inputs",
        home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "terminal-inputs".into(),
        context_config(),
    )?;
    runtime
        .enqueue(MessageEnvelope::new(
            "terminal-inputs",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "qualified terminal".into(),
            },
        ))
        .await?;
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime).poll().await?
    else {
        anyhow::bail!("source was not claimed")
    };
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
    let state = runtime.agent_state().await?;
    let terminal = TurnTerminalRecord {
        turn_id: state
            .current_turn_id
            .clone()
            .context("started Turn missing")?,
        turn_index: state.turn_index,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: None,
        no_brief_reason: None,
        checkpoint: None,
        completed_at: runtime.now(),
        duration_ms: 1,
    };
    Ok((runtime, home, terminal))
}

fn tool(terminal: &TurnTerminalRecord, index: usize) -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: format!("terminal-tool-{index:03}"),
        agent_id: "terminal-inputs".into(),
        work_item_id: None,
        turn_id: Some(terminal.turn_id.clone()),
        turn_index: terminal.turn_index,
        tool_name: "GenerateImage".into(),
        created_at: terminal.completed_at,
        completed_at: Some(terminal.completed_at),
        duration_ms: 1,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({}),
        output: serde_json::json!({"artifact": "x".repeat(8192)}),
        summary: "generated attachment".into(),
        invocation_surface: None,
    }
}

#[tokio::test]
async fn qualified_terminal_full_members_are_sql_free_and_held_through_last_clone() -> Result<()> {
    let (runtime, _home, terminal) = started().await?;
    for index in 0..70 {
        runtime
            .persist_tool_execution_evidence(&tool(&terminal, index))
            .await?;
    }
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let read = runtime.prepare_turn_record_read(&terminal.turn_id).await?;
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(before.token, after.token);
    assert!(Arc::ptr_eq(
        before.input_evidence.as_ref().unwrap(),
        after.input_evidence.as_ref().unwrap(),
    ));
    match (&before.current, &after.current) {
        (Some(before), Some(after)) => assert!(Arc::ptr_eq(before, after)),
        (None, None) => {}
        _ => panic!("terminal read changed Current"),
    }
    drop((before, after));
    assert_eq!(read.members.tools.len(), 70);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(crate::types::AuditEvent::legacy(
            "terminal-cut-test",
            serde_json::json!({}),
        ))
        .await?;
    assert!(old_root.upgrade().is_none());
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE messages RENAME TO terminal_forbidden_messages;
             ALTER TABLE briefs RENAME TO terminal_forbidden_briefs;
             ALTER TABLE tool_executions RENAME TO terminal_forbidden_tools;
             ALTER TABLE turn_records RENAME TO terminal_forbidden_turns;
             ALTER TABLE wait_conditions RENAME TO terminal_forbidden_waits;",
        )?;
        Ok(())
    })?;
    let record = RuntimeHandle::build_turn_record_from_inputs(read, &terminal)?;
    assert_eq!(
        record.tool_execution_ids,
        (0..70)
            .map(|index| format!("terminal-tool-{index:03}"))
            .collect::<Vec<_>>()
    );
    let source = Arc::downgrade(record._source.as_ref().unwrap());
    let last = record.try_clone()?;
    drop(record);
    assert!(source.upgrade().is_some());
    assert_eq!(last.tool_execution_ids.len(), 70);
    drop(last);
    assert!(source.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn qualified_terminal_refuses_raw_and_changed_members_without_partial_publication(
) -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError;
    let (runtime, _home, terminal) = started().await?;
    let raw = tool(&terminal, 0);
    runtime.storage().append_tool_execution(&raw)?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let token = root.token;
    drop(root);
    let error = runtime
        .prepare_turn_record_read(&terminal.turn_id)
        .await
        .err()
        .context("raw Tool was accepted")?;
    assert!(matches!(
        error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::NeedsPreparation(_))
    ));
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "DELETE FROM tool_executions WHERE evidence_id=?1",
            [&raw.id],
        )?;
        Ok(())
    })?;
    runtime.persist_tool_execution_evidence(&raw).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let token = root.token;
    drop(root);
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE tool_executions SET payload_json=json_set(payload_json,'$.summary','changed outside writer')
             WHERE evidence_id=?1", [&raw.id],
        )?;
        Ok(())
    })?;
    let error = runtime
        .prepare_turn_record_read(&terminal.turn_id)
        .await
        .err()
        .context("changed Tool was accepted")?;
    assert!(matches!(
        error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ProjectionChanged(_))
    ));
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    Ok(())
}

#[tokio::test]
async fn finite_terminal_read_rejects_unacknowledged_turn_deletion() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError;
    let (runtime, _home, terminal) = started().await?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "DELETE FROM turn_records WHERE turn_id=?1",
            [&terminal.turn_id],
        )?;
        Ok(())
    })?;
    let error = runtime
        .prepare_turn_record_read(&terminal.turn_id)
        .await
        .err()
        .context("raw deletion became an optional empty Turn")?;
    assert!(matches!(
        error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ProjectionChanged(_))
    ));
    Ok(())
}
