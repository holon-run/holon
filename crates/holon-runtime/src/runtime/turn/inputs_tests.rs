use super::*;
use crate::{
    runtime::{
        scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor},
        tests::support::{context_config, tempdir, StubProvider},
    },
    types::{
        AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, ToolExecutionRecord,
        ToolExecutionStatus, TurnNoBriefReason, TurnTerminalKind, TurnTerminalRecord,
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
        call_binding: None,
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

#[tokio::test]
async fn retired_destructive_fence_rewrite_fails_turn_reads_until_repair() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError;

    fn payload_digest(payload: &str) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(payload.as_bytes()))
    }

    fn no_runtime_backfill(
        _: &rusqlite::Transaction<'_>,
        _: holon_storage::migrations::RuntimeBackfill,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    // Re-freeze every Turn-family fingerprint for this turn against the
    // current payload, the way the pre-v121 runtime did.
    fn refreeze_turn_evidence(tx: &rusqlite::Transaction<'_>, turn_id: &str) -> Result<()> {
        let payload: String = tx.query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
            [turn_id],
            |row| row.get(0),
        )?;
        let bytes = i64::try_from(payload.len()).context("payload exceeds i64")?;
        let digest = payload_digest(&payload);
        for (sql, bindings) in [
            (
                "UPDATE input_evidence_current
                 SET content_digest = ?1,
                     fingerprint_json = json_set(fingerprint_json, '$.payload_bytes', ?2)
                 WHERE family = 'turn' AND record_id = ?3",
                rusqlite::params![digest, bytes, turn_id],
            ),
            (
                "UPDATE input_evidence_eligibility
                 SET fingerprint_json = json_set(
                   fingerprint_json, '$.payload_bytes', ?1, '$.frozen_payload_digest', ?2)
                 WHERE family = 'turn' AND record_id = ?3",
                rusqlite::params![bytes, digest, turn_id],
            ),
            (
                "UPDATE input_evidence_seed_recovery_current
                 SET fingerprint_json = json_set(
                   fingerprint_json, '$.payload_bytes', ?1, '$.frozen_payload_digest', ?2)
                 WHERE family = 'turn' AND record_id = ?3",
                rusqlite::params![bytes, digest, turn_id],
            ),
        ] {
            tx.execute(sql, bindings)?;
        }
        Ok(())
    }

    let (runtime, _home, terminal) = started().await?;
    let turn_id = terminal.turn_id.clone();

    // Settle the started dispatch so a later queued input may deliver.
    runtime
        .persist_turn_terminal_record(
            TurnTerminalKind::Completed,
            None,
            Some(TurnNoBriefReason::ToolOnlyWait),
            1,
            None,
            true,
        )
        .await?;

    // Sanity: the completed Turn reads cleanly before any drift.
    runtime.prepare_turn_record_read(&turn_id).await?;

    // v117-era state: the payload still carries the retired fence key and
    // the frozen evidence matches it.
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE turn_records
             SET payload_json = json_set(
               payload_json, '$.destructive_operation_id', 'legacy-fence')
             WHERE turn_id = ?1",
            [&turn_id],
        )?;
        refreeze_turn_evidence(tx, &turn_id)?;
        Ok(())
    })?;
    runtime.prepare_turn_record_read(&turn_id).await?;

    // Replay the v121 fence retirement rewrite.
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "UPDATE turn_records
             SET payload_json = CASE
               WHEN terminal_kind IS NULL THEN json_remove(
                 json_set(payload_json, '$.external_side_effect_possible', json('true')),
                 '$.destructive_operation_id')
               ELSE json_remove(payload_json, '$.destructive_operation_id')
             END
             WHERE json_extract(payload_json, '$.destructive_operation_id') IS NOT NULL;",
        )?;
        Ok(())
    })?;

    // The unfixed chain leaves the frozen fingerprint drifted, so turn input
    // preparation fails closed exactly like production.
    let error = runtime
        .prepare_turn_record_read(&turn_id)
        .await
        .err()
        .context("drifted Turn input was accepted")?;
    assert!(matches!(
        error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ProjectionChanged(_))
    ));

    // Re-run the real v123 repair migration against the drifted rows.
    let migration = holon_storage::migrations::MIGRATIONS
        .iter()
        .find(|migration| {
            migration.name == "repair_retired_destructive_operation_turn_fingerprints"
        })
        .unwrap();
    let mut connection = runtime.runtime_db().connection()?;
    connection.execute("DELETE FROM schema_migrations WHERE version = 123", [])?;
    holon_storage::migrations::apply_migration(&mut connection, migration, no_runtime_backfill)?;
    drop(connection);

    // Repaired evidence passes the real read path again.
    runtime.prepare_turn_record_read(&turn_id).await?;

    // With the read path restored, queued inputs face no projection gate:
    // turn input preparation is the exact failure surface that quarantined
    // agents and pinned queued recovery turns in production.
    Ok(())
}
