use super::*;
use crate::runtime_db::command_task_creation::OriginalCommandTaskCreation;

fn original(id: &str) -> TaskRecord {
    let mut original = task(id, TaskStatus::Queued);
    original.detail = Some(serde_json::json!({
        "rejoin_obligation_id":id,"rejoin_generation":1,"cmd":"true",
    }));
    original
}

fn creator(id: &str) -> Result<OriginalCommandTaskCreation> {
    OriginalCommandTaskCreation::new(scope(), original(id))
}

fn create(tx: &Transaction<'_>, creation: &OriginalCommandTaskCreation) -> Result<()> {
    creation.write_tx(tx)?;
    write_task(tx, creation.task())?;
    Ok(())
}

fn assert_turnless(tx: &Transaction<'_>, id: &str) -> Result<()> {
    let snapshot = prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
    let fence = snapshot.resident.coverage().task_metadata[id]
        .rejoin
        .as_ref()
        .unwrap();
    assert_eq!(fence.source.command_creation_id(), Some(id));
    assert!(fence.source.parent_turn_id().is_none());
    assert!(!snapshot
        .input_roots
        .iter()
        .any(|key| key.family == InputEvidenceFamily::Turn));
    assert!(snapshot.turnless_parent_turns.is_empty());
    Ok(())
}

#[test]
fn original_command_creation_survives_plain_read_cancellation_and_restart() -> Result<()> {
    let (dir, db) = fresh()?;
    let creation = creator("command:positive-creation")?;
    db.transaction(|tx| create(tx, &creation))?;
    db.transaction(|tx| assert_turnless(tx, &creation.task().id))?;
    db.transaction(|tx| {
        let mut cancelled = creation.task().clone();
        cancelled.status = TaskStatus::Cancelled;
        cancelled.updated_at = at() + Duration::seconds(1);
        write_task(tx, &cancelled)?;
        assert_turnless(tx, &cancelled.id)
    })?;
    drop(db);
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| assert_turnless(tx, &creation.task().id))?;
    Ok(())
}

#[test]
fn turnless_terminal_result_has_positive_creation_not_future_turn_root() -> Result<()> {
    let (_dir, db) = fresh()?;
    let creation = creator("command:terminal-source")?;
    db.transaction(|tx| create(tx, &creation))?;
    db.transaction(|tx| {
        let mut terminal = creation.task().clone();
        terminal.status = TaskStatus::Completed;
        terminal.updated_at = at() + Duration::seconds(1);
        terminal.detail.as_mut().unwrap()["result_execution_turn_id"] =
            serde_json::json!("turn:future-execution-not-a-parent");
        let mut message = MessageEnvelope::new(
            "task-agent",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: terminal.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "completed".into(),
            },
        );
        message.id = "message:positive-command-result".into();
        message.task_id = Some(terminal.id.clone());
        message.turn_id = Some("turn:future-execution-not-a-parent".into());
        message.created_at = terminal.updated_at;
        let (message, _) = append_message_tx(tx, &message)?;
        terminal.parent_message_id = Some(message.id.clone());
        let result = TaskResultSettlementRecord::pending(&terminal, &message, terminal.updated_at)?
            .context("positive creation must produce a pending Result")?;
        let keys = BTreeSet::from([
            TaskKey::new(TaskFamily::Task, &terminal.id),
            TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
        ]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &terminal)?;
        upsert_pending_tx(tx, &result)?;
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &request(),
        )?;
        let snapshot =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert!(matches!(
            snapshot.resident.coverage().result_dependencies[&result.result_identity].relation,
            TaskResultDependencyRelation::QualifiedCurrent { .. }
        ));
        assert_eq!(
            result.source.command_creation_id(),
            Some(terminal.id.as_str())
        );
        assert_turnless(tx, &terminal.id)
    })
}

#[test]
fn raw_creation_reference_without_original_writer_is_rejected() -> Result<()> {
    let (_dir, db) = fresh()?;
    let creation = creator("command:raw-detail-is-not-authority")?;
    let error = db
        .transaction(|tx| {
            write_task(tx, creation.task())?;
            assert_turnless(tx, &creation.task().id)
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("Command creation source"));
    Ok(())
}

#[test]
fn creation_source_rejects_other_task_incarnation_generation_and_body() -> Result<()> {
    let (_dir, db) = fresh()?;
    let creation = creator("command:exact-origin")?;
    db.transaction(|tx| create(tx, &creation))?;
    db.transaction(|tx| {
        let original = creation.task();
        let fence = original.rejoin_fence().unwrap();
        let validate = |scope: &InputEvidenceScope, id: &str, generation, task: &TaskRecord| {
            crate::runtime_db::command_task_creation::validate_source_tx(
                tx,
                scope,
                id,
                generation,
                &fence.source,
                None,
                Some(task),
                |_, _| Ok(()),
            )
        };
        validate(&scope(), &original.id, 1, original)?;
        let mut other = scope();
        other.identity_incarnation += 1;
        assert!(validate(&other, &original.id, 1, original).is_err());
        let mut foreign = scope();
        foreign.agent_id = "other-agent".into();
        assert!(validate(&foreign, &original.id, 1, original).is_err());
        assert!(validate(&scope(), "command:other", 1, original).is_err());
        assert!(validate(&scope(), &original.id, 2, original).is_err());
        let mut changed = original.clone();
        changed.created_at += Duration::seconds(1);
        assert!(validate(&scope(), &original.id, 1, &changed).is_err());
        assert!(creation.write_tx(tx).is_err());
        Ok(())
    })
}

#[test]
fn creator_rejects_parented_or_non_command_and_current_parent_omission() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut parented = original("command:has-parent");
    parented.detail.as_mut().unwrap()["parent_turn_id"] = serde_json::json!("turn:actual-parent");
    assert!(OriginalCommandTaskCreation::new(scope(), parented).is_err());
    let mut non_command = original("command:not-command");
    non_command.kind = TaskKind::AgentMessageWait;
    assert!(OriginalCommandTaskCreation::new(scope(), non_command).is_err());
    let creation = creator("command:must-be-turnless")?;
    db.transaction(|tx| {
        let mut state = AgentState::new("task-agent");
        state.current_turn_id = Some("turn:actual-parent".into());
        upsert_agent_state_tx(tx, &state)?;
        assert!(creation.write_tx(tx).is_err());
        Ok(())
    })
}

#[test]
fn original_creation_and_task_rollback_together() -> Result<()> {
    let (_dir, db) = fresh()?;
    let creation = creator("command:atomic-origin")?;
    let failure: Result<()> = db.transaction(|tx| {
        create(tx, &creation)?;
        anyhow::bail!("injected after original creation and Task")
    });
    assert!(failure.is_err());
    db.transaction(|tx| {
        let counts: (u64, u64) = tx.query_row(
            "SELECT (SELECT count(*) FROM command_task_creation_sources WHERE task_id=?1),
                    (SELECT count(*) FROM tasks WHERE task_id=?1)",
            [&creation.task().id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(counts, (0, 0));
        create(tx, &creation)
    })
}
