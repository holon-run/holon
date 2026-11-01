use super::*;
use crate::runtime_db::transitions::{
    InputMutationExecution, TurnTerminalTransitionCommand, WaitTransitionCommand,
};
use crate::runtime_db::{
    migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
    repositories::upsert_queue_entry_tx,
    retention::RuntimeDbRetentionPolicy,
};
use crate::types::{
    TranscriptEntry, TranscriptEntryKind, TurnTerminalKind, TurnTerminalRecord,
    TurnTerminalSummary, WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource,
};
use std::sync::Arc;

fn state(f: &Fixture) -> Result<AgentState> {
    f.db.transaction(|tx| {
        ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .context("State missing")
    })
}
fn update(f: &Fixture, nonce: &str, objective: &str) -> Result<PreparedInputTransition> {
    prepare_update(f, nonce, objective, true)
}
fn prepare_update(
    f: &Fixture,
    nonce: &str,
    objective: &str,
    deferred: bool,
) -> Result<PreparedInputTransition> {
    let mut work =
        f.db.work_items()
            .latest("work:dispatch-memory")?
            .context("Work missing")?;
    let expected_revision = work.revision;
    work.revision += 1;
    work.updated_at += chrono::Duration::milliseconds(1);
    work.objective = objective.into();
    let prepared = PreparedInputTransition::work(
        f.scope.clone(),
        nonce.into(),
        f.at,
        WorkItemTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            mutation: WorkItemMutation::Update {
                record: work,
                expected_revision,
            },
            agent_state: None,
            brief_evidence: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: true,
            fault: None,
        },
        Default::default(),
        false,
    )?;
    if deferred {
        prepared.with_dispatch_memory(DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen(nonce),
        })
    } else {
        Ok(prepared)
    }
}
#[test]
fn original_posture_receipt_binds_small_seed_address_and_preserves_replay() -> Result<()> {
    let f = Fixture::new(true)?;
    f.db.transaction(|tx| {
        let actual = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        let binding = load_dispatch_memory_binding_tx(tx, &f.scope, &actual)?;
        assert_eq!(binding, Some(DispatchMemoryBinding::from_start(&f.start)));
        validate_dispatch_memory_members_tx(
            tx,
            &load_dispatch_memory_tx(tx, f.start.key(), f.start.invocation())?.unwrap(),
        )?;
        Ok(())
    })?;
    let original = prepare_start_command(&f.db, &f.scope, &f.start, f.at)?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
        InputMutationExecution::Replayed { .. }
    ));
    let mut later = state(&f)?;
    later.pending = 9;
    f.db.agent_states().upsert_for_fixture(&later)?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(state(&f)?, later);
    Ok(())
}
#[test]
fn actual_detached_work_receipts_keep_first_frozen_baseline_and_gate_new_claim() -> Result<()> {
    let f = Fixture::new(true)?;
    let first = update(&f, "first-member", "first actual objective")?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&first)?;
    let original = f.read()?.unwrap();
    assert_eq!(
        original.obligation().unwrap().frozen(),
        &f.frozen("first-member")
    );
    assert_eq!(state(&f)?.working_memory, *f.start.baseline());
    let second = update(&f, "second-member", "second actual objective")?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&second)?;
    let pending = f.read()?.unwrap();
    assert_eq!(
        pending.obligation().unwrap().frozen(),
        &f.frozen("first-member")
    );
    assert_eq!(pending.obligation().unwrap().members.len(), 2);
    f.db.transaction(|tx| {
        validate_dispatch_memory_members_tx(tx, &pending)?;
        assert!(validate_new_dispatch_tx(tx, &f.scope).is_err());
        Ok(())
    })?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&first)?,
        InputMutationExecution::Replayed { .. }
    ));
    Ok(())
}

fn terminal(f: &Fixture, nonce: &str) -> Result<PreparedInputTransition> {
    terminal_command(f, nonce, true)
}
fn terminal_command(f: &Fixture, nonce: &str, finish: bool) -> Result<PreparedInputTransition> {
    let before = state(f)?;
    let mut after = before.clone();
    after.status = AgentStatus::AwakeIdle;
    after.current_run_id = None;
    let terminal = TurnTerminalRecord {
        turn_id: f.start.key.actual_dispatch_turn_id.clone(),
        turn_index: 1,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: None,
        no_brief_reason: None,
        checkpoint: None,
        completed_at: f.at,
        duration_ms: 1,
    };
    after.last_turn_terminal = Some(terminal.clone());
    let mut turn =
        f.db.transaction(|tx| {
            ContextDurableRead::new(tx, &f.scope.agent_id).turn_by_id(&terminal.turn_id)
        })?
        .unwrap();
    turn.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
    let prepared = PreparedInputTransition::terminal(
        f.scope.clone(),
        nonce.into(),
        f.at,
        TurnTerminalTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(before)),
                record: Box::new(after),
            },
            turn_record: turn,
            terminal_tool_executions: Vec::new(),
            audit_events: Vec::new(),
            fault: None,
        },
    )?;
    if finish {
        prepared.with_dispatch_memory(DispatchMemoryIntent::Finish {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen(nonce),
        })
    } else {
        Ok(prepared)
    }
}
fn receipt_count(f: &Fixture) -> Result<usize> {
    f.db.connection()?
        .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |r| {
            r.get(0)
        })
        .map_err(Into::into)
}

#[test]
fn actual_terminal_finishes_once_then_done_binding_allows_later_work() -> Result<()> {
    let f = Fixture::new(true)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&update(
            &f,
            "detached-before-terminal",
            "retained exact work",
        )?)?;
    let original = terminal(&f, "finish-original")?;
    let InputMutationExecution::Committed { receipt, .. } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?
    else {
        panic!("first finish must commit")
    };
    assert!(matches!(f.read()?, Some(DispatchMemoryRecord::Done { .. })));
    let after = state(&f)?;
    assert_eq!(after.status, AgentStatus::AwakeIdle);
    assert!(after.current_run_id.is_none());
    assert_eq!(
        after.current_turn_id,
        Some(f.start.key.actual_dispatch_turn_id.clone())
    );
    f.db.transaction(|tx| {
        assert!(load_dispatch_memory_binding_tx(tx, &f.scope, &after)?.is_none());
        assert!(load_dispatch_memory_binding_by_turn_tx(
            tx,
            &f.scope,
            &f.start.key.actual_dispatch_turn_id
        )?
        .is_none());
        assert!(!is_legacy_dispatch_tx(
            tx,
            f.start.key(),
            f.start.invocation()
        )?);
        validate_new_dispatch_tx(tx, &f.scope)
    })?;
    let d1 =
        f.db.episode_memory_receipt(f.start.key(), None, f.start.invocation())?
            .unwrap();
    let revision: u64 = f.db.connection()?.query_row(
        "SELECT control_revision FROM agent_states WHERE agent_id=?1",
        [&f.scope.agent_id],
        |r| r.get(0),
    )?;
    assert_eq!(d1.agent_control_revision, revision);
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepare_update(
            &f,
            "after-done",
            "later no-dispatch objective",
            false,
        )?)?;
    let later = state(&f)?;
    assert_eq!(
        later.working_memory, after.working_memory,
        "NoTurn Work must keep the D1 checkpoint"
    );
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?
    else {
        panic!("exact outer replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(state(&f)?, later);
    f.db.transaction(|tx| {
        assert!(load_dispatch_memory_binding_tx(tx, &f.scope, &later)?.is_none());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn whole_receipt_failure_rolls_back_terminal_memory_done_and_all_receipts() -> Result<()> {
    let f = Fixture::new(true)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&update(
            &f,
            "before-failed-finish",
            "first original change",
        )?)?;
    let before = state(&f)?;
    let pending = f.read()?;
    let count = receipt_count(&f)?;
    let original = terminal(&f, "failed-whole-finish")?;
    f.db.connection()?.execute_batch("CREATE TRIGGER fail_outer_memory_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='failed-whole-finish' BEGIN SELECT RAISE(ABORT,'outer whole receipt failure');END;")?;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)
            .unwrap_err();
    assert!(format!("{error:#}").contains("outer whole receipt failure"));
    assert_eq!(state(&f)?, before);
    assert_eq!(f.read()?, pending);
    assert_eq!(receipt_count(&f)?, count);
    assert!(f
        .db
        .episode_memory_receipt(f.start.key(), None, f.start.invocation())?
        .is_none());
    assert!(f
        .db
        .transaction(|tx| ContextDurableRead::new(tx, &f.scope.agent_id)
            .turn_by_id(&f.start.key.actual_dispatch_turn_id))?
        .unwrap()
        .terminal
        .is_none());
    Ok(())
}

#[test]
fn missing_new_start_is_not_legacy_and_original_whole_replay_precedes_gate() -> Result<()> {
    let f = Fixture::new(true)?;
    let original = prepare_start_command(&f.db, &f.scope, &f.start, f.at)?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
        InputMutationExecution::Replayed { .. }
    ));
    let mut proposed = f.start.clone();
    proposed.key.actual_dispatch_turn_id = "turn:new-dispatch".into();
    proposed.invocation.turn_index = 2;
    proposed.digest = proposed.compute_digest()?;
    // The original receipt bypasses a prior unfinished phase. A new Started
    // command is blocked before any new Turn/body can be persisted.
    let current = state(&f)?;
    let mut next = current.clone();
    next.turn_index = 2;
    next.current_turn_id = Some(proposed.key.actual_dispatch_turn_id.clone());
    next.current_execution_binding.as_mut().unwrap().turn_id =
        proposed.key.actual_dispatch_turn_id.clone();
    let mut turn = TurnRecord::new(&f.scope.agent_id, &proposed.key.actual_dispatch_turn_id, 2);
    turn.created_at = f.at;
    turn.run_id = next.current_run_id.clone();
    turn.input_message_ids = vec![f.source.id.clone()];
    let command = PreparedInputTransition::posture(
        f.scope.clone(),
        "blocked-new-dispatch".into(),
        f.at,
        AgentPostureTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            expected_incarnation: Some(1),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(current.clone())),
                record: Box::new(next),
            },
            audit_events: Vec::new(),
            fault: None,
        },
        Some(turn),
    )?
    .with_dispatch_memory(DispatchMemoryIntent::Started {
        start: Box::new(proposed),
    })?;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&command)
            .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DispatchMemoryError>(),
        Some(DispatchMemoryError::Unfinished)
    ));
    assert_eq!(state(&f)?, current);
    f.db.transaction(|tx| {
        tx.execute("DELETE FROM dispatch_memory_checkpoints", [])?;
        let error = load_dispatch_memory_binding_tx(tx, &f.scope, &current).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<DispatchMemoryError>(),
            Some(DispatchMemoryError::MissingStart)
        ));
        Ok(())
    })?;
    Ok(())
}

#[test]
fn real_fixed_upgrade_only_freezes_original_actual_dispatch_incarnation() -> Result<()> {
    let existing = Fixture::new(false)?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("legacy.sqlite");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut connection, migration)?;
    }
    {
        let tx = connection.transaction()?;
        let identity = existing
            .db
            .transaction(|tx| ContextDurableRead::new(tx, &existing.scope.agent_id).scope_facts())?
            .identity
            .unwrap();
        upsert_agent_identity_tx(&tx, &identity)?;
        let (source, _) = append_message_tx(&tx, &existing.source)?;
        assert_eq!(source.message_seq, existing.source.message_seq);
        let turn = existing
            .db
            .transaction(|tx| {
                ContextDurableRead::new(tx, &existing.scope.agent_id)
                    .turn_by_id(&existing.start.key.actual_dispatch_turn_id)
            })?
            .unwrap();
        upsert_turn_record_tx(&tx, &turn)?;
        upsert_agent_state_tx(&tx, &state(&existing)?)?;
        tx.commit()?;
    }
    for migration in MIGRATIONS.iter().filter(|m| m.version > 89) {
        apply_migration(&mut connection, migration)?;
    }
    drop(connection);
    let db = RuntimeDb::open_and_migrate(&path, directory.path().join("legacy.lock"))?;
    // SQLite migration fixes eligibility without replaying old file storage.
    db.initialize_sqlite_storage_domains()?;
    db.transaction(|tx| {
        let current = ContextDurableRead::new(tx, &existing.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        assert!(load_dispatch_memory_binding_tx(tx, &existing.scope, &current)?.is_none());
        assert!(is_legacy_dispatch_tx(
            tx,
            existing.start.key(),
            existing.start.invocation()
        )?);
        let successor = EpisodeMemoryKey {
            identity_incarnation: 2,
            ..existing.start.key.clone()
        };
        assert!(!is_legacy_dispatch_tx(
            tx,
            &successor,
            existing.start.invocation()
        )?);
        assert!(
            load_dispatch_memory_tx(tx, existing.start.key(), existing.start.invocation())?
                .is_none()
        );
        Ok(())
    })?;
    // A subsequent real Started command consumes the fixed marker. Deleting
    // that new seed cannot turn a post-upgrade command back into legacy.
    let start = prepare_start_command(&db, &existing.scope, &existing.start, existing.at)?;
    db.transitions_in_scope(&existing.scope)
        .commit_prepared_transition(&start)?;
    db.transaction(|tx| {
        assert!(!is_legacy_dispatch_tx(
            tx,
            existing.start.key(),
            existing.start.invocation()
        )?);
        tx.execute("DELETE FROM dispatch_memory_checkpoints", [])?;
        let current = ContextDurableRead::new(tx, &existing.scope.agent_id)
            .scope_facts()?
            .into_owned_state()?
            .unwrap();
        assert!(load_dispatch_memory_binding_tx(tx, &existing.scope, &current).is_err());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn fixed_dispatch_upgrade_freezes_missing_tables_without_late_legacy_adoption() -> Result<()> {
    let migration = MIGRATIONS
        .iter()
        .find(|m| m.name == "actual_dispatch_memory_responsibility")
        .unwrap();
    for table in [
        "agent_states",
        "agent_identities",
        "turn_records",
        "messages",
    ] {
        let f = Fixture::new(false)?;
        let mut connection = f.db.connection()?;
        connection.execute(
            "DELETE FROM schema_migrations WHERE version=?1",
            [migration.version],
        )?;
        connection.execute_batch(&format!(
            "ALTER TABLE {table} RENAME TO absent_dispatch_source"
        ))?;
        apply_migration(&mut connection, migration)?;
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM dispatch_memory_legacy_dispatches",
                [],
                |row| row.get::<_, usize>(0),
            )?,
            0,
            "missing {table} cannot prove an original dispatch"
        );
        connection.execute_batch(&format!(
            "ALTER TABLE absent_dispatch_source RENAME TO {table}"
        ))?;
        // Normal migration retry observes its committed boundary, not late rows.
        apply_migration(&mut connection, migration)?;
        drop(connection);
        f.db.transaction(|tx| {
            assert!(!is_legacy_dispatch_tx(
                tx,
                f.start.key(),
                f.start.invocation()
            )?);
            let current = ContextDurableRead::new(tx, &f.scope.agent_id)
                .scope_facts()?
                .into_owned_state()?
                .unwrap();
            let error = load_dispatch_memory_binding_tx(tx, &f.scope, &current).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<DispatchMemoryError>(),
                Some(DispatchMemoryError::MissingStart)
            ));
            Ok(())
        })?;
    }
    Ok(())
}

fn wait(f: &Fixture, id: &str) -> WaitConditionRecord {
    WaitConditionRecord {
        id: id.into(),
        agent_id: f.scope.agent_id.clone(),
        work_item_id: Some("work:dispatch-memory".into()),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Operator,
        source: Some("original actual dispatch".into()),
        subject_ref: None,
        waiting_for: "operator evidence".into(),
        wake_sources: vec![WakeSource::OperatorInput],
        continuation: None,
        created_at: f.at,
        updated_at: f.at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: Some(f.start.key.actual_dispatch_turn_id.clone()),
        trigger_message_id: None,
        triggered_at: None,
    }
}
fn registration(f: &Fixture, id: &str) -> WaitTransitionCommand {
    WaitTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        work_items: Vec::new(),
        expected_wait_conditions: Vec::new(),
        wait_conditions: vec![wait(f, id)],
        timer_wake: None,
        task_result_admission: None,
        agent_state: None,
        audit_events: Vec::new(),
        index_changes: Vec::new(),
        notify_scheduler: true,
        fault: None,
    }
}
#[test]
fn bound_queue_wait_and_memory_finish_share_the_original_whole_receipt() -> Result<()> {
    let f = Fixture::new(true)?;
    let dequeued = QueueEntryRecord {
        message_id: f.source.id.clone(),
        agent_id: f.scope.agent_id.clone(),
        priority: f.source.priority.clone(),
        status: QueueEntryStatus::Dequeued,
        created_at: f.source.created_at,
        updated_at: f.at,
    };
    // The fixture already owns the real canonical Start/assignment. Seed the
    // corresponding admitted queue checkpoint; this test exercises settlement.
    f.db.transaction(|tx| upsert_queue_entry_tx(tx, &dequeued))?;
    let before = state(&f)?;
    let mut after = before.clone();
    after.current_run_id = None;
    after.status = AgentStatus::AwakeIdle;
    after.current_work_item_id = Some("work:dispatch-memory".into());
    let terminal = TurnTerminalRecord {
        turn_id: f.start.key.actual_dispatch_turn_id.clone(),
        turn_index: 1,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: None,
        no_brief_reason: None,
        checkpoint: None,
        completed_at: f.at,
        duration_ms: 0,
    };
    after.last_turn_terminal = Some(terminal.clone());
    let mut turn =
        f.db.transaction(|tx| {
            ContextDurableRead::new(tx, &f.scope.agent_id).turn_by_id(&terminal.turn_id)
        })?
        .unwrap();
    turn.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
    let mut processed = dequeued.clone();
    processed.status = QueueEntryStatus::Processed;
    let mut queue = PreparedQueueTransition::new(QueueTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        operation: QueueOperation::Settle,
        mutation: QueueMutation::CompareAndSet {
            expected: dequeued,
            record: processed,
        },
        scheduler_claim_work_item: None,
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(before.clone())),
            record: Box::new(after),
        }),
        message_evidence: Vec::new(),
        transcript_entries: Vec::new(),
        turn_record: Some(turn),
        audit_events: Vec::new(),
        notify_scheduler: true,
        fault: None,
        brief_evidence: Vec::new(),
    });
    queue.wait_registration = Some(registration(&f, "wait:bound-finish"));
    let prepared = PreparedInputTransition::queue(
        f.scope.clone(),
        "bound-original-queue".into(),
        f.at,
        queue,
    )?
    .with_dispatch_memory(DispatchMemoryIntent::Finish {
        binding: DispatchMemoryBinding::from_start(&f.start),
        frozen: f.frozen("bound-original-queue"),
    })?;
    let pending = f.read()?;
    let count = receipt_count(&f)?;
    f.db.connection()?.execute_batch("CREATE TRIGGER fail_bound_whole_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='bound-original-queue' BEGIN SELECT RAISE(ABORT,'bound whole receipt failure'); END;")?;
    let failure =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)
            .unwrap_err();
    assert!(format!("{failure:#}").contains("bound whole receipt failure"));
    assert_eq!(state(&f)?, before);
    assert_eq!(f.read()?, pending);
    assert_eq!(receipt_count(&f)?, count);
    f.db.transaction(|tx| {
        let status: String = tx.query_row(
            "SELECT status FROM queue_entries WHERE message_id=?1",
            [&f.source.id],
            |r| r.get(0),
        )?;
        assert_eq!(status, "dequeued");
        let waits: usize = tx.query_row(
            "SELECT COUNT(*) FROM wait_conditions WHERE wait_condition_id='wait:bound-finish'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(waits, 0);
        assert!(ContextDurableRead::new(tx, &f.scope.agent_id)
            .turn_by_id(&f.start.key.actual_dispatch_turn_id)?
            .unwrap()
            .terminal
            .is_none());
        Ok(())
    })?;
    assert!(f
        .db
        .episode_memory_receipt(f.start.key(), None, f.start.invocation())?
        .is_none());
    f.db.connection()?
        .execute_batch("DROP TRIGGER fail_bound_whole_receipt;")?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("first original queue commit")
    };
    assert!(value
        .effects
        .work_delta
        .as_ref()
        .unwrap()
        .coverage
        .unresolved_wait_ids
        .contains("wait:bound-finish"));
    assert!(matches!(f.read()?, Some(DispatchMemoryRecord::Done { .. })));
    let after = state(&f)?;
    assert!(after
        .working_memory
        .current_working_memory
        .waiting_on
        .iter()
        .any(|v| v.contains("operator evidence")));
    assert!(f
        .db
        .episode_memory_receipt(f.start.key(), None, f.start.invocation())?
        .is_some());
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("bound original replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(state(&f)?, after);
    Ok(())
}
fn deferred_queue_member(f: &Fixture) -> Result<PreparedInputTransition> {
    let mut message = f.source.clone();
    message.id = "message:detached-member".into();
    message.message_seq = None;
    message.turn_id = None;
    let before = state(f)?;
    let mut after = before.clone();
    after.pending = 1;
    let mut trace = TranscriptEntry::new(
        &f.scope.agent_id,
        TranscriptEntryKind::RuntimeFailure,
        None,
        None,
        serde_json::json!({"message":"exact detached trace"}),
    );
    trace.id = "transcript:member-only-root".into();
    trace.created_at = f.at;
    let mut queue = PreparedQueueTransition::new(QueueTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        operation: QueueOperation::Admit,
        mutation: QueueMutation::Upsert(QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: f.scope.agent_id.clone(),
            priority: message.priority.clone(),
            status: QueueEntryStatus::Queued,
            created_at: message.created_at,
            updated_at: f.at,
        }),
        scheduler_claim_work_item: None,
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(before)),
            record: Box::new(after),
        }),
        message_evidence: vec![message],
        transcript_entries: vec![trace],
        turn_record: None,
        audit_events: Vec::new(),
        notify_scheduler: true,
        fault: None,
        brief_evidence: Vec::new(),
    });
    queue.wait_registration = Some(registration(f, "wait:deferred-root"));
    PreparedInputTransition::queue(f.scope.clone(), "retained-queue-member".into(), f.at, queue)?
        .with_dispatch_memory(DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("retained-queue-member"),
        })
}
fn retention_policy() -> RuntimeDbRetentionPolicy {
    RuntimeDbRetentionPolicy {
        enabled: true,
        transcript_entries_days: 1,
        tool_executions_days: 1,
        transcript_entries_min_rows: 1,
        tool_executions_min_rows: 1,
        ..Default::default()
    }
}
#[test]
fn original_member_receipt_roots_are_used_by_actual_retention_and_done_releases_them() -> Result<()>
{
    let f = Fixture::new(true)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&deferred_queue_member(&f)?)?;
    let key = InputEvidenceKey::new(
        InputEvidenceFamily::Transcript,
        "transcript:member-only-root",
    );
    assert!(f
        .db
        .transaction(|tx| dispatch_memory_retention_roots_in(tx))?
        .contains(&key));
    let mut recent = TranscriptEntry::new(
        &f.scope.agent_id,
        TranscriptEntryKind::RuntimeFailure,
        None,
        None,
        serde_json::json!({"message":"minimum floor padding"}),
    );
    recent.id = "transcript:recent-floor".into();
    recent.created_at = f.at + chrono::Duration::days(30);
    f.db.evidence()
        .append_transcript_entry_in_scope(&f.scope, &recent)?;
    let now = f.at + chrono::Duration::days(31);
    let before = f.db.plan_retention(retention_policy(), now)?;
    assert_eq!(before.transcript_entries.planned_delete_rows, 0);
    assert_eq!(
        before
            .transcript_entries
            .protection_reasons
            .get("transcript_ref"),
        Some(&1)
    );
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&terminal(&f, "retention-finish")?)?;
    assert!(!f
        .db
        .transaction(|tx| dispatch_memory_retention_roots_in(tx))?
        .contains(&key));
    let after = f.db.plan_retention(retention_policy(), now)?;
    assert_eq!(after.transcript_entries.planned_delete_rows, 1);
    Ok(())
}
#[test]
fn mutable_pending_record_without_its_original_whole_receipt_cannot_authorize_retention(
) -> Result<()> {
    let f = Fixture::new(true)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&deferred_queue_member(&f)?)?;
    f.db.connection()?.execute(
        "DELETE FROM input_mutation_receipts WHERE command_nonce='retained-queue-member'",
        [],
    )?;
    let error =
        f.db.plan_retention(retention_policy(), f.at + chrono::Duration::days(31))
            .unwrap_err();
    assert!(format!("{error:#}").contains("no original whole-transaction receipt"));
    assert!(f.read()?.unwrap().obligation().is_some());
    Ok(())
}

fn memory_request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    }
}
fn memory_ledger(
) -> Result<std::sync::Arc<crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger>> {
    // History pressure is separate; Candidate observation has no hidden shared
    // hard cap and does not borrow the 32/512MiB reclaimable History contract.
    let mut limits = crate::config::StateMemoryLimits::default();
    limits.history_per_agent_bytes = 1;
    limits.history_host_bytes = 1;
    Ok(std::sync::Arc::new(
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(limits)?,
    ))
}
#[test]
fn transaction_owned_seed_token_guards_observe_actual_material_and_release() -> Result<()> {
    use crate::runtime::input_evidence_budget::InputMemoryCategory;
    let f = Fixture::new(true)?;
    let ledger = memory_ledger()?;
    let before = ledger.report()?.owned;
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Finish {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("observed-material"),
        };
        let budget =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &memory_request(), &ledger)?;
        let report = ledger.report()?;
        assert!(report.owned.payload_bytes > 0);
        assert!(
            report
                .categories
                .get(&InputMemoryCategory::Candidate)
                .unwrap()
                .bytes
                > 1
        );
        ledger.check_history_budget()?;
        let token = prepare_dispatch_memory_finish_tx(
            tx,
            &DispatchMemoryBinding::from_start(&f.start),
            &f.frozen("observed-material"),
        )?;
        assert!(ledger.report()?.owned.payload_bytes >= report.owned.payload_bytes);
        drop(token);
        drop(budget);
        assert_eq!(ledger.report()?.owned, before);
        Ok(())
    })?;
    Ok(())
}
#[test]
fn combined_memory_preparation_limit_rolls_back_before_any_business_or_receipt() -> Result<()> {
    let f = Fixture::new(true)?;
    let ledger = memory_ledger()?;
    let before = state(&f)?;
    let checkpoint = f.read()?;
    let work = f.db.work_items().latest("work:dispatch-memory")?;
    let count = receipt_count(&f)?;
    let audits: usize =
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM audit_events", [], |r| r.get(0))?;
    let maximum_single:usize=f.db.connection()?.query_row(
        "SELECT MAX(size) FROM (SELECT length(CAST(payload_json AS BLOB)) size FROM agent_states UNION ALL SELECT length(CAST(payload_json AS BLOB)) FROM dispatch_memory_checkpoints UNION ALL SELECT length(CAST(payload_json AS BLOB)) FROM messages UNION ALL SELECT length(CAST(payload_json AS BLOB)) FROM turn_records)",[],|r|r.get(0))?;
    // Each original body is individually within this limit. Their simultaneously
    // live seed/token/State/material is not granted one separate cap per helper.
    let mut request = memory_request();
    request.byte_limit = maximum_single + 1;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &update(&f, "combined-too-large", "must roll back")?,
                &request,
                &ledger,
                &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                    recovery_at: None,
                    query: Default::default(),
                    capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                    record_limit: 65_536,
                    byte_limit: 64 * 1024 * 1024,
                },
            )
            .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DispatchMemoryError>(),
        Some(DispatchMemoryError::ResourceExhausted)
    ));
    assert_eq!(state(&f)?, before);
    assert_eq!(f.read()?, checkpoint);
    assert_eq!(f.db.work_items().latest("work:dispatch-memory")?, work);
    assert_eq!(receipt_count(&f)?, count);
    assert_eq!(
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM audit_events", [], |r| r
                .get::<_, usize>(0))?,
        audits
    );
    assert_eq!(ledger.report()?.owned, Default::default());
    // Same original command can still commit with its original intent and the
    // real larger acquisition allowance; the failed attempt created no ACK.
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &update(&f, "combined-too-large", "must roll back")?,
            &memory_request(),
            &ledger,
            &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit: 65_536,
                byte_limit: 64 * 1024 * 1024,
            },
        )?;
    assert_eq!(ledger.report()?.owned, Default::default());
    Ok(())
}
#[test]
fn original_turn_binding_survives_cleared_current_fields_but_does_not_authorize_finish(
) -> Result<()> {
    let f = Fixture::new(true)?;
    let mut cleared = state(&f)?;
    cleared.current_execution_binding = None;
    cleared.current_turn_id = None;
    cleared.current_run_id = None;
    f.db.agent_states().upsert_for_fixture(&cleared)?;
    f.db.transaction(|tx| {
        assert!(load_dispatch_memory_binding_tx(tx, &f.scope, &cleared)?.is_none());
        assert_eq!(
            load_dispatch_memory_binding_by_turn_tx(
                tx,
                &f.scope,
                &f.start.key.actual_dispatch_turn_id
            )?,
            Some(DispatchMemoryBinding::from_start(&f.start))
        );
        // The address is not producer-ended authority or a new dispatch permit.
        assert!(matches!(
            validate_new_dispatch_tx(tx, &f.scope)
                .unwrap_err()
                .downcast_ref::<DispatchMemoryError>(),
            Some(DispatchMemoryError::Unfinished)
        ));
        let wrong = InputEvidenceScope {
            agent_id: f.scope.agent_id.clone(),
            identity_incarnation: 2,
        };
        assert!(load_dispatch_memory_binding_by_turn_tx(
            tx,
            &wrong,
            &f.start.key.actual_dispatch_turn_id
        )
        .is_err());
        tx.execute("DELETE FROM dispatch_memory_checkpoints", [])?;
        assert!(matches!(
            load_dispatch_memory_binding_by_turn_tx(
                tx,
                &f.scope,
                &f.start.key.actual_dispatch_turn_id
            )
            .unwrap_err()
            .downcast_ref::<DispatchMemoryError>(),
            Some(DispatchMemoryError::MissingStart)
        ));
        Ok(())
    })?;
    Ok(())
}
#[test]
fn new_actual_queue_claim_is_blocked_before_sql_while_start_receipt_replay_is_legal() -> Result<()>
{
    let f = Fixture::new(true)?;
    let original = prepare_start_command(&f.db, &f.scope, &f.start, f.at)?;
    let before = state(&f)?;
    let mut message = f.source.clone();
    message.id = "message:next-claim".into();
    message.message_seq = None;
    let queued = QueueEntryRecord {
        message_id: message.id.clone(),
        agent_id: f.scope.agent_id.clone(),
        priority: message.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: f.at,
        updated_at: f.at,
    };
    let mut pending = before.clone();
    pending.pending = 1;
    let admit = PreparedInputTransition::queue(
        f.scope.clone(),
        "admit-next-before-gate".into(),
        f.at,
        PreparedQueueTransition::new(QueueTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            operation: QueueOperation::Admit,
            mutation: QueueMutation::Upsert(queued.clone()),
            scheduler_claim_work_item: None,
            agent_state: Some(AgentStateMutation {
                expected: Some(Box::new(before)),
                record: Box::new(pending),
            }),
            message_evidence: vec![message],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        }),
    )?;
    assert!(!admit.dispatch_memory_candidate());
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&admit)?;
    let before = state(&f)?;
    let mut dequeued = queued.clone();
    dequeued.status = QueueEntryStatus::Dequeued;
    let claim = PreparedInputTransition::queue(
        f.scope.clone(),
        "blocked-next-claim".into(),
        f.at,
        PreparedQueueTransition::new(QueueTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            operation: QueueOperation::Claim,
            mutation: QueueMutation::Consume(dequeued),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        }),
    )?;
    assert!(!claim.dispatch_memory_candidate());
    let count = receipt_count(&f)?;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&claim)
            .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DispatchMemoryError>(),
        Some(DispatchMemoryError::Unfinished)
    ));
    assert_eq!(state(&f)?, before);
    assert_eq!(receipt_count(&f)?, count);
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT status FROM queue_entries WHERE message_id=?1",
            [&queued.message_id],
            |r| r.get::<_, String>(0)
        )?,
        "queued"
    );
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(state(&f)?, before);
    Ok(())
}

#[test]
fn business_terminal_noop_still_finishes_original_memory_and_writes_whole_receipt() -> Result<()> {
    let f = Fixture::new(true)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&terminal_command(&f, "canonical-terminal-first", false)?)?;
    let before = state(&f)?;
    assert!(f
        .db
        .episode_memory_receipt(f.start.key(), None, f.start.invocation())?
        .is_none());
    assert!(matches!(
        f.read()?,
        Some(DispatchMemoryRecord::Started { .. })
    ));
    let prepared = terminal(&f, "finish-after-business-noop")?;
    let InputMutationExecution::Committed { value, receipt } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("first memory finish requires whole ACK")
    };
    assert!(
        value.effects.work_delta.is_some(),
        "no-op finalizer must return its exact final Work view"
    );
    assert!(matches!(f.read()?, Some(DispatchMemoryRecord::Done { .. })));
    let after = state(&f)?;
    let mut non_memory = after.clone();
    non_memory.working_memory = before.working_memory.clone();
    assert_eq!(
        non_memory, before,
        "Memory finish must not change non-WM business fields"
    );
    assert!(f
        .db
        .episode_memory_receipt(f.start.key(), None, f.start.invocation())?
        .is_some());
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("exact whole replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(state(&f)?, after);
    Ok(())
}

#[test]
fn nonempty_final_work_observations_are_metered_and_released() -> Result<()> {
    let f = Fixture::new(true)?;
    let ledger = memory_ledger()?;
    let before = ledger.report()?.owned;
    f.db.transaction(|tx| {
        let request = memory_request();
        let selected = crate::runtime_db::work_snapshot::preflight_work_domain_snapshot_tx(
            tx, &f.scope, &request,
        )?
        .require_current()?
        .resident
        .capture(&request.capture)?;
        assert!(!selected.records.is_empty());
        assert!(!selected.observations.is_empty());
        let intent = DispatchMemoryIntent::Finish {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("nonempty-work-material"),
        };
        let mut budget =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &request, &ledger)?;
        let seed_bytes = ledger.report()?.owned.payload_bytes;
        let work_bytes = selected.records.iter().try_fold(0u64, |total, record| {
            Ok::<_, anyhow::Error>(
                total + crate::runtime::input_evidence_budget::serialized_bytes(&record.record)?,
            )
        })?;
        // Independently preserve the old sequence encoding and exact 48-copy allowance.
        let metadata_bytes = crate::runtime::input_evidence_budget::serialized_bytes(&(
            &selected.scope,
            selected.revision,
            selected.live_complete,
            &selected.live_work_ids,
            &selected.unresolved_wait_ids,
            &selected.active_frame_ids,
            &selected.turn_wait_keys,
            &selected.task_wait_keys,
            &selected.trigger_wait_keys,
            &selected.nonopen,
            &selected.latest_results,
            selected.observations.iter().collect::<Vec<_>>(),
        ))?;
        let admitted = budget.capture_final_work(tx, &selected)?;
        assert!(std::ptr::eq(admitted.work(), &selected));
        admitted.validate_closure_scope(&f.scope.agent_id)?;
        assert_eq!(
            admitted
                .validate_closure_scope("foreign-final-work-owner")
                .unwrap_err()
                .to_string(),
            "closure differs from original admitted final Work scope",
        );
        let material =
            admitted.audit_material(tx, &f.scope.agent_id, f.scope.identity_incarnation)?;
        assert_eq!(
            admitted
                .audit_material(tx, &f.scope.agent_id, f.scope.identity_incarnation + 1)
                .unwrap_err()
                .to_string(),
            "Audit material differs from original finalizer incarnation",
        );
        assert!(admitted
            .audit_material(tx, "foreign-final-work-owner", f.scope.identity_incarnation)
            .is_err());
        let mut other = f.db.connection()?;
        let other_tx = other.transaction()?;
        assert_eq!(
            admitted
                .audit_material(&other_tx, &f.scope.agent_id, f.scope.identity_incarnation)
                .unwrap_err()
                .to_string(),
            "Audit material changed original finalizer transaction",
        );
        // Only the Arc header is allocated; the existing guard Vec moves intact.
        let shared_header = (std::mem::size_of::<
            Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
        >() + 2 * std::mem::size_of::<usize>()) as u64;
        assert_eq!(
            ledger.report()?.owned.payload_bytes,
            seed_bytes + (work_bytes + metadata_bytes) * 48 + shared_header
        );
        drop(admitted);
        assert_eq!(
            ledger.report()?.owned.payload_bytes,
            seed_bytes + (work_bytes + metadata_bytes) * 48 + shared_header
        );
        let weak = Arc::downgrade(&material);
        drop(budget);
        assert!(weak.upgrade().is_some());
        assert!(ledger.report()?.owned.payload_bytes > seed_bytes);
        drop(material);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.owned, before);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn new_work_memory_amplification_is_preflighted_before_planner_and_whole_ack() -> Result<()> {
    let f = Fixture::new(true)?;
    let objective = "x".repeat(512 * 1024);
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepare_update(
            &f,
            "large-current-work",
            &objective,
            false,
        )?)?;
    // Real owned Open Work exists and is qualified. Keep the old memory small,
    // so only the new derived snapshot/planner growth exercises this limit.
    let mut focused = state(&f)?;
    focused.current_work_item_id = Some("work:dispatch-memory".into());
    f.db.agent_states().upsert_for_fixture(&focused)?;
    let before = state(&f)?;
    let checkpoint = f.read()?;
    let count = receipt_count(&f)?;
    let turn = f.db.turn_records().by_id(
        Some(&f.scope.agent_id),
        &f.start.key.actual_dispatch_turn_id,
    )?;
    let ledger = memory_ledger()?;
    let prepared = terminal(&f, "large-work-finish")?;
    let mut request = memory_request();
    request.byte_limit = 16 * 1024 * 1024;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &prepared,
                &request,
                &ledger,
                &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                    recovery_at: None,
                    query: Default::default(),
                    capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                    record_limit: 65_536,
                    byte_limit: 64 * 1024 * 1024,
                },
            )
            .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DispatchMemoryError>(),
        Some(DispatchMemoryError::ResourceExhausted)
    ));
    assert_eq!(state(&f)?, before);
    assert_eq!(f.read()?, checkpoint);
    assert_eq!(
        f.db.turn_records().by_id(
            Some(&f.scope.agent_id),
            &f.start.key.actual_dispatch_turn_id
        )?,
        turn
    );
    assert_eq!(receipt_count(&f)?, count);
    assert!(f
        .db
        .episode_memory_receipt(f.start.key(), None, f.start.invocation())?
        .is_none());
    assert_eq!(ledger.report()?.owned, Default::default());
    // The original intent is still retryable with a combined budget that fits.
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            &prepared,
            &memory_request(),
            &ledger,
            &crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                record_limit: 65_536,
                byte_limit: 64 * 1024 * 1024,
            },
        )?;
    assert_eq!(
        state(&f)?
            .working_memory
            .current_working_memory
            .objective
            .as_deref(),
        Some(objective.as_str())
    );
    assert!(matches!(f.read()?, Some(DispatchMemoryRecord::Done { .. })));
    assert_eq!(ledger.report()?.owned, Default::default());
    Ok(())
}
