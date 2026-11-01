use super::super::*;
use super::support::*;

fn timer_execution_snapshot(
    runtime: &RuntimeHandle,
) -> crate::domain::execution_protocol::ExecutionProtocolState {
    runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap()
}

// These recovery cases model canonical rows from before the fixed D3 cutover.
// Current-schema raw inserts deliberately cannot acquire legacy eligibility.
fn seed_legacy_timer_backlog(
    data_dir: &std::path::Path,
    timers: &[TimerRecord],
    messages: &[MessageEnvelope],
) {
    seed_legacy_timer_backlog_with_wait(data_dir, timers, messages, None);
}

fn seed_legacy_timer_backlog_with_wait(
    data_dir: &std::path::Path,
    timers: &[TimerRecord],
    messages: &[MessageEnvelope],
    work_wait: Option<(&WorkItemRecord, &WaitConditionRecord, &AgentState)>,
) {
    use crate::runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        repositories::{enum_string, timestamp, upsert_queue_entry_tx, upsert_timer_tx},
    };
    let path = data_dir.join(".holon/state/runtime.sqlite");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut connection = rusqlite::Connection::open(path).unwrap();
    ensure_migration_table(&connection).unwrap();
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= 81)
    {
        apply_migration(&mut connection, migration).unwrap();
    }
    let tx = connection.transaction().unwrap();
    let identity = crate::types::AgentIdentityRecord::new(
        "default",
        crate::types::AgentKind::Default,
        crate::types::AgentVisibility::Public,
        crate::types::AgentOwnership::SelfOwned,
        crate::types::AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    upsert_agent_identity_tx(&tx, &identity).unwrap();
    let mut state = AgentState::new("default");
    state.pending = messages.len();
    state.total_message_count = messages.len();
    upsert_agent_state_tx(&tx, &state).unwrap();
    if let Some((work, wait, state)) = work_wait {
        // Use v81's original columns; current writers require post-migration projections.
        tx.execute(
            "INSERT INTO work_items (work_item_id, agent_id, state, objective, plan_status, revision, current_focus, created_at, updated_at, completed_at, plan_artifact_path, last_turn_id, payload_json) VALUES (?1, ?2, 'open', ?3, ?4, ?5, 0, ?6, ?7, NULL, NULL, NULL, ?8)",
            rusqlite::params![
                work.id, work.agent_id, work.objective, enum_string(&work.plan_status).unwrap(),
                work.revision as i64, timestamp(work.created_at), timestamp(work.updated_at),
                serde_json::to_string(work).unwrap(),
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO wait_conditions (wait_condition_id,agent_id,work_item_id,status,kind,source,subject_ref,waiting_for,created_at,updated_at,expires_at,resolved_at,cancelled_at,last_turn_id,trigger_message_id,triggered_at,payload_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                wait.id, wait.agent_id, wait.work_item_id, enum_string(&wait.status).unwrap(),
                enum_string(&wait.kind).unwrap(), wait.source, wait.subject_ref, wait.waiting_for,
                timestamp(wait.created_at), timestamp(wait.updated_at), wait.expires_at.map(timestamp),
                wait.resolved_at.map(timestamp), wait.cancelled_at.map(timestamp), wait.turn_id,
                wait.trigger_message_id, wait.triggered_at.map(timestamp), serde_json::to_string(wait).unwrap(),
            ],
        )
        .unwrap();
        upsert_agent_state_tx(&tx, state).unwrap();
    }
    for timer in timers {
        upsert_timer_tx(&tx, timer).unwrap();
    }
    for message in messages {
        let message = append_message_tx(&tx, message).unwrap().0;
        upsert_queue_entry_tx(
            &tx,
            &QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority,
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: message.created_at,
            },
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

// Keep the test task runnable so paused Tokio cannot jump to the old deadline.
async fn yield_timer_loop() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

fn legacy_timer_queue_status(
    runtime: &RuntimeHandle,
    timer: &TimerRecord,
    status: QueueEntryStatus,
) -> QueueEntryRecord {
    let command = terminal_timer_queue_command(runtime, timer);
    let crate::runtime_db::transitions::QueueMutation::CompareAndSet {
        record: mut entry, ..
    } = command.mutation
    else {
        unreachable!();
    };
    entry.status = status;
    // Simulate a database written before terminal queue/timer composition.
    runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute(
            "UPDATE queue_entries SET status = ?2, payload_json = ?3 WHERE message_id = ?1",
            rusqlite::params![
                entry.message_id,
                serde_json::to_value(&entry.status)
                    .unwrap()
                    .as_str()
                    .unwrap(),
                serde_json::to_string(&entry).unwrap()
            ],
        )
        .unwrap();
    entry
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_restart_retires_orphan_pending_idempotently() {
    let (dir, workspace, runtime, timer) = terminal_timer_fixture(true).await;
    let entry = legacy_timer_queue_status(&runtime, &timer, QueueEntryStatus::Dropped);
    let recovered = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("must not run")),
        "default".into(),
        context_config(),
        controlled_clock(),
    )
    .unwrap();
    assert!(recovered
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
    assert_eq!(timer_retirement_events(&recovered).len(), 1);
    assert_eq!(
        timer_retirement_events(&recovered)[0].data["message_id"],
        entry.message_id
    );
    let again = recovered
        .inner
        .runtime_db
        .timers()
        .normalize_wakes_for_recovery("default", recovered.now())
        .unwrap();
    assert_eq!(again.invalidated_wakes, 0);
    assert_eq!(timer_retirement_events(&recovered).len(), 1);
    let mut timer = timer;
    recovered.fire_timer_record(&mut timer).await.unwrap();
    assert_eq!(
        recovered
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .unwrap()
            .fire_count,
        2
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_recovery_preserves_live_queue_and_parked_sources() {
    for status in [
        QueueEntryStatus::Queued,
        QueueEntryStatus::Interrupted,
        QueueEntryStatus::Dequeued,
        QueueEntryStatus::Interjected,
    ] {
        let (_dir, _workspace, runtime, timer) = terminal_timer_fixture(true).await;
        let entry = legacy_timer_queue_status(&runtime, &timer, status);
        runtime
            .inner
            .runtime_db
            .timers()
            .normalize_wakes_for_recovery("default", runtime.now())
            .unwrap();
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .timers()
                .pending_wake(&timer.id)
                .unwrap()
                .unwrap()
                .message_id,
            entry.message_id
        );
    }

    let (_dir, _workspace, runtime, timer) = terminal_timer_fixture(true).await;
    let message_id = assert_single_durable_timer_wake(&runtime, &timer.id);
    let parking = crate::runtime_db::provider_admission::ProviderQueueParking {
        agent_id: "default".into(),
        message_id: message_id.clone(),
        chain_fingerprint: "timer-chain".into(),
        episode_key: "timer-episode".into(),
        not_before: runtime.now() + chrono::Duration::hours(1),
        reason: "chain_cooling".into(),
    };
    runtime
        .inner
        .runtime_db
        .park_provider_queue(&parking)
        .unwrap();
    runtime
        .inner
        .runtime_db
        .transitions()
        .commit_queue(&terminal_timer_queue_command(&runtime, &timer))
        .unwrap();
    runtime
        .inner
        .runtime_db
        .timers()
        .normalize_wakes_for_recovery("default", runtime.now())
        .unwrap();
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_some());
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .provider_queue_parking("default", &message_id)
            .unwrap(),
        Some(parking)
    );
    assert!(timer_retirement_events(&runtime).is_empty());
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_recovery_preserves_unsettled_turn_consumption() {
    for interrupted in [false, true] {
        let (_dir, _workspace, runtime, timer) = terminal_timer_fixture(true).await;
        let message_id = assert_single_durable_timer_wake(&runtime, &timer.id);
        let mut turn = TurnRecord::new("default", "retained-timer-turn", 1);
        turn.input_message_ids = vec![message_id.clone()];
        if interrupted {
            turn.terminal = Some(crate::types::TurnTerminalSummary {
                kind: crate::types::TurnTerminalKind::Interrupted,
                reason: None,
                no_brief_reason: None,
                completed_at: runtime.now(),
                duration_ms: 0,
            });
        }
        runtime.storage().append_turn(&turn).unwrap();
        runtime
            .inner
            .runtime_db
            .transitions()
            .commit_queue(&terminal_timer_queue_command(&runtime, &timer))
            .unwrap();
        runtime
            .inner
            .runtime_db
            .timers()
            .normalize_wakes_for_recovery("default", runtime.now())
            .unwrap();
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .timers()
                .pending_wake(&timer.id)
                .unwrap()
                .unwrap()
                .message_id,
            message_id
        );
        assert!(timer_retirement_events(&runtime).is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_settles_registered_wait_without_resuming_work() {
    for work_item_scope in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let clock = controlled_clock();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            clock.clone(),
        )
        .unwrap();
        let work_id = if work_item_scope {
            Some(
                runtime
                    .create_work_item("timer wait".into(), None, None, Vec::new())
                    .await
                    .unwrap()
                    .id,
            )
        } else {
            None
        };
        let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
        let registration = runtime
            .register_wait_for(
                "default",
                work_id.clone(),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "timer dependency".into(),
                Some(120_000),
            )
            .await
            .unwrap();
        let before_execution = work_id.as_ref().map(|_| timer_execution_snapshot(&runtime));
        clock.rewind(std::time::Duration::from_secs(1));
        let cancellation =
            crate::tool::tools::timer::cancel(&runtime, &serde_json::json!({"timer_id": timer.id}))
                .await
                .unwrap();
        let evidence = cancellation.envelope.result.as_ref().unwrap();
        assert_eq!(evidence["id"], timer.id);
        assert_eq!(
            evidence["cancelled_wait_condition_ids"],
            serde_json::json!([registration.condition.id])
        );
        let wait = runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|wait| wait.id == registration.condition.id)
            .unwrap();
        assert_eq!(wait.status, WaitConditionStatus::Cancelled);
        assert_eq!(wait.cancelled_at, Some(clock.now()));
        assert_eq!(wait.updated_at, registration.condition.updated_at);
        assert_eq!(
            wait.continuation.as_ref().unwrap()["terminal_reason"],
            "timer_cancelled"
        );
        assert!(runtime
            .storage()
            .raw_unresolved_wait_conditions_for_agent("default")
            .unwrap()
            .is_empty());
        if let Some(work_id) = work_id {
            let work = runtime
                .storage()
                .latest_work_item(&work_id)
                .unwrap()
                .unwrap();
            assert_eq!(work.state, WorkItemState::Open);
            assert!(work
                .blocked_by
                .as_deref()
                .unwrap()
                .contains("timer_cancelled"));
            assert!(work.blocked_by_wait_id.is_none());
            assert!(work.recheck_at.is_none());
            let execution = runtime
                .inner
                .runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized("default")
                .unwrap()
                .unwrap();
            assert_eq!(
                execution.work_items[&work_id].source_revision, work.revision,
                "timer cancellation must advance the registered source in its transaction"
            );
            assert_eq!(
                execution.work_items[&work_id].state,
                before_execution.as_ref().unwrap().work_items[&work_id].state,
                "cancellation is not a readiness or generation transition"
            );
            let written = runtime
                .storage()
                .read_recent_events(100)
                .unwrap()
                .into_iter()
                .filter(|event| {
                    event.kind == "work_item_written"
                        && event.data["action"] == "timer_cancelled"
                        && event.data["work_item_id"] == work_id
                })
                .collect::<Vec<_>>();
            assert_eq!(written.len(), 1);
            assert_eq!(written[0].data["revision"], work.revision);
            assert_eq!(written[0].data["expected_revision"], work.revision - 1);
        }
        assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
        let before = runtime.storage().latest_wait_conditions().unwrap();
        runtime.cancel_timer(&timer.id).await.unwrap();
        assert_eq!(runtime.storage().latest_wait_conditions().unwrap(), before);
        clock.advance(std::time::Duration::from_secs(180));
        tokio::time::advance(std::time::Duration::from_secs(180)).await;
        yield_timer_loop().await;
        assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
        assert!(!runtime
            .storage()
            .read_recent_messages(100)
            .unwrap()
            .iter()
            .any(|message| message.kind == MessageKind::TimerTick));
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_startup_recovery_preserves_source_revision() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("must not run")),
        "default".into(),
        context_config(),
        controlled_clock(),
    )
    .unwrap();
    let work = runtime
        .create_work_item("recover cancelled timer".into(), None, None, Vec::new())
        .await
        .unwrap();
    let mut timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
    let registration = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "timer dependency".into(),
            Some(120_000),
        )
        .await
        .unwrap();
    // Simulate an older writer that persisted cancellation before settling its waits.
    timer.status = TimerStatus::Cancelled;
    runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute(
            "UPDATE timers SET status = 'cancelled', payload_json = ?2 WHERE timer_id = ?1",
            rusqlite::params![timer.id, serde_json::to_string(&timer).unwrap()],
        )
        .unwrap();
    let recovered = runtime
        .inner
        .runtime_db
        .timers()
        .normalize_wakes_for_recovery("default", runtime.now())
        .unwrap();
    assert_eq!(recovered.settled_wait_conditions.len(), 1);
    assert_eq!(
        recovered.settled_wait_conditions[0].id,
        registration.condition.id
    );
    let updated = runtime
        .storage()
        .latest_work_item(&work.id)
        .unwrap()
        .unwrap();
    let execution = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        execution.work_items[&work.id].source_revision, updated.revision,
        "startup cancellation must use the same revision invariant as the online writer"
    );
    let before = runtime.storage().read_recent_events(100).unwrap();
    let repeated = runtime
        .inner
        .runtime_db
        .timers()
        .normalize_wakes_for_recovery("default", runtime.now())
        .unwrap();
    assert!(repeated.settled_wait_conditions.is_empty());
    assert!(repeated.work_items.is_empty());
    assert_eq!(runtime.storage().read_recent_events(100).unwrap(), before);
    assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_preserves_missing_terminal_and_drifted_registration_boundaries() {
    use crate::domain::execution_protocol::WorkItemExecutionState;

    for boundary in ["missing", "terminal", "source_behind", "source_ahead"] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            controlled_clock(),
        )
        .unwrap();
        let work = runtime
            .create_work_item(
                format!("cancel boundary {boundary}"),
                None,
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
        runtime
            .register_wait_for(
                "default",
                Some(work.id.clone()),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "timer dependency".into(),
                Some(120_000),
            )
            .await
            .unwrap();
        let mut execution = timer_execution_snapshot(&runtime);
        if boundary == "missing" {
            runtime
                .inner
                .runtime_db
                .connection()
                .unwrap()
                .execute(
                    "DELETE FROM execution_protocol_work_items WHERE work_item_id = ?1",
                    [&work.id],
                )
                .unwrap();
            execution.work_items.remove(&work.id);
        } else {
            let registered = execution.work_items.get_mut(&work.id).unwrap();
            match boundary {
                "terminal" => {
                    registered.state = WorkItemExecutionState::Terminal {
                        generation: registered.generation(),
                        completion: "test historical terminal".into(),
                    };
                }
                "source_behind" => registered.source_revision -= 1,
                "source_ahead" => registered.source_revision += 1,
                _ => unreachable!(),
            }
            runtime
                .inner
                .runtime_db
                .transaction(|tx| {
                    crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx(
                        tx, &execution,
                    )
                })
                .unwrap();
        }
        let before_work = runtime
            .storage()
            .latest_work_item(&work.id)
            .unwrap()
            .unwrap();
        let before_waits = runtime.storage().latest_wait_conditions().unwrap();
        let before_events = runtime.storage().read_recent_events(100).unwrap();
        let result = runtime.cancel_timer(&timer.id).await;
        if boundary == "missing" {
            result.unwrap();
            assert!(!timer_execution_snapshot(&runtime)
                .work_items
                .contains_key(&work.id));
            assert_eq!(
                runtime
                    .storage()
                    .latest_work_item(&work.id)
                    .unwrap()
                    .unwrap()
                    .revision,
                before_work.revision + 1
            );
        } else {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("source revision"),
                "{boundary} must fail closed: {error}"
            );
            assert_eq!(
                runtime.latest_timer(&timer.id).await.unwrap().unwrap(),
                timer
            );
            assert_eq!(
                runtime.storage().latest_work_item(&work.id).unwrap(),
                Some(before_work)
            );
            assert_eq!(
                runtime.storage().latest_wait_conditions().unwrap(),
                before_waits
            );
            assert_eq!(
                runtime.storage().read_recent_events(100).unwrap(),
                before_events
            );
            assert_eq!(timer_execution_snapshot(&runtime), execution);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_fences_prepared_wait_commit() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("must not run")),
        "default".into(),
        context_config(),
        controlled_clock(),
    )
    .unwrap();
    let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
    let PrepareWaitForOutcome::Prepared(prepared) = runtime
        .prepare_wait_for_outcome(
            "default",
            None,
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "prepared timer wait".into(),
            None,
        )
        .await
        .unwrap()
    else {
        panic!("timer wait must be prepared");
    };
    runtime.cancel_timer(&timer.id).await.unwrap();
    let error = runtime
        .inner
        .runtime_db
        .transitions()
        .commit_wait(&prepared.command)
        .unwrap_err();
    assert!(error
        .downcast_ref::<crate::runtime_db::RuntimeStateTransitionConflict>()
        .is_some());
    assert!(runtime
        .storage()
        .raw_unresolved_wait_conditions_for_agent("default")
        .unwrap()
        .is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_fences_waiting_report_transaction() {
    use crate::runtime_db::transitions::{
        ExecutionProtocolTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
    };
    for cancel in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            controlled_clock(),
        )
        .unwrap();
        let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
        let PrepareWaitForOutcome::Prepared(prepared) = runtime
            .prepare_wait_for_outcome(
                "default",
                None,
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "waiting report settlement".into(),
                None,
            )
            .await
            .unwrap()
        else {
            panic!("timer wait must be prepared");
        };
        let source = MessageEnvelope::new(
            "default",
            MessageKind::TimerTick,
            MessageOrigin::Timer {
                timer_id: timer.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "source turn".into(),
            },
        );
        let brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "waiting for timer",
            Some(source.id.clone()),
            None,
        );
        let command = QueueTransitionCommand {
            agent_id: "default".into(),
            operation: QueueOperation::Settle,
            mutation: QueueMutation::Upsert(QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: "default".into(),
                priority: Priority::Next,
                status: QueueEntryStatus::Processed,
                created_at: source.created_at,
                updated_at: source.created_at,
            }),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: vec![source.clone()],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: vec![brief.clone()],
        };
        if cancel {
            runtime.cancel_timer(&timer.id).await.unwrap();
        }
        let result = runtime
            .inner
            .runtime_db
            .transitions()
            .commit_queue_terminal_with_wait(
                &command,
                &ExecutionProtocolTransition::default(),
                &prepared.command,
                None,
                &[],
                None,
            );
        if cancel {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("timer_wait"), "{error:#}");
            assert!(runtime
                .storage()
                .read_brief_by_id(&brief.id)
                .unwrap()
                .is_none());
            assert!(runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&source.id)
                .unwrap()
                .is_none());
            assert!(runtime
                .storage()
                .raw_unresolved_wait_conditions_for_agent("default")
                .unwrap()
                .is_empty());
        } else {
            assert!(result.unwrap().applied);
            assert!(runtime
                .storage()
                .read_brief_by_id(&brief.id)
                .unwrap()
                .is_some());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_settles_only_matching_waits_and_owned_blockers() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("must not run")),
        "default".into(),
        context_config(),
        controlled_clock(),
    )
    .unwrap();
    let timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
    let unrelated_timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
    let mut matching = Vec::new();
    let mut manual_blocker = None;
    for index in 0..2 {
        let work = runtime
            .create_work_item(format!("wait {index}"), None, None, Vec::new())
            .await
            .unwrap();
        let registration = runtime
            .register_wait_for(
                "default",
                Some(work.id.clone()),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "matching dependency".into(),
                Some(120_000),
            )
            .await
            .unwrap();
        matching.push(registration.condition);
        if index == 1 {
            let mut work = runtime
                .storage()
                .latest_work_item(&work.id)
                .unwrap()
                .unwrap();
            work.blocked_by = Some("independent manual blocker".into());
            work.blocked_by_wait_id = None;
            let expected_revision = work.revision;
            work.revision += 1;
            let commit = {
                let mut mutation = runtime.begin_mutation().await.unwrap();
                mutation
                    .commit_work_transition(
                        crate::ids::audit_event_id(),
                        crate::runtime_db::transitions::WorkItemTransitionCommand {
                            agent_id: work.agent_id.clone(),
                            mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                                record: work.clone(),
                                expected_revision,
                            },
                            agent_state: None,
                            brief_evidence: Vec::new(),
                            audit_events: Vec::new(),
                            index_changes: Vec::new(),
                            notify_scheduler: false,
                            fault: None,
                        },
                        Default::default(),
                        true,
                    )
                    .await
                    .unwrap()
            };
            runtime.apply_transition_commit(commit).await;
            manual_blocker = Some(work);
        }
    }
    let unrelated = runtime
        .register_wait_for(
            "default",
            None,
            WaitForWakeKind::Timer,
            Some(unrelated_timer.id.clone()),
            "unrelated timer".into(),
            None,
        )
        .await
        .unwrap()
        .condition;
    // A malformed legacy foreign-agent reference must not expand cancellation scope.
    let mut foreign = matching[0].clone();
    foreign.id = "foreign-timer-wait".into();
    foreign.agent_id = "another-agent".into();
    foreign.work_item_id = None;
    runtime.storage().append_wait_condition(&foreign).unwrap();
    runtime.cancel_timer(&timer.id).await.unwrap();
    let waits = runtime.storage().latest_wait_conditions().unwrap();
    for expected in &matching {
        assert_eq!(
            waits
                .iter()
                .find(|wait| wait.id == expected.id)
                .unwrap()
                .status,
            WaitConditionStatus::Cancelled
        );
    }
    assert_eq!(
        waits.iter().find(|wait| wait.id == foreign.id).unwrap(),
        &foreign
    );
    assert_eq!(
        waits.iter().find(|wait| wait.id == unrelated.id).unwrap(),
        &unrelated
    );
    let manual_blocker = manual_blocker.unwrap();
    assert_eq!(
        runtime
            .storage()
            .latest_work_item(&manual_blocker.id)
            .unwrap()
            .unwrap(),
        manual_blocker
    );
    let projected = runtime
        .latest_work_item(matching[0].work_item_id.as_ref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(projected
        .blocked_by
        .as_deref()
        .unwrap()
        .contains("timer_cancelled"));
    assert!(projected.blocked_by_wait_id.is_none());
}

#[tokio::test(start_paused = true)]
async fn timer_fire_between_prepare_and_commit_remains_waitable() {
    for repeat in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            controlled_clock(),
        )
        .unwrap();
        let mut timer = runtime
            .schedule_timer(60_000, repeat.then_some(60_000), None)
            .await
            .unwrap();
        let PrepareWaitForOutcome::Prepared(prepared) = runtime
            .prepare_wait_for_outcome(
                "default",
                None,
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "timer may fire during preparation".into(),
                None,
            )
            .await
            .unwrap()
        else {
            panic!("timer wait must be prepared");
        };
        runtime.fire_timer_record(&mut timer).await.unwrap();
        runtime
            .inner
            .runtime_db
            .transitions()
            .commit_wait(&prepared.command)
            .unwrap();
        assert!(runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .is_some());
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_respects_wake_admission_boundary() {
    for (admitted, work_item_scope) in [(false, false), (true, false), (false, true), (true, true)]
    {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            controlled_clock(),
        )
        .unwrap();
        let work_item_id = if work_item_scope {
            Some(
                runtime
                    .create_work_item("cancel admission".into(), None, None, Vec::new())
                    .await
                    .unwrap()
                    .id,
            )
        } else {
            None
        };
        let mut timer = runtime
            .schedule_timer(60_000, Some(60_000), None)
            .await
            .unwrap();
        let registered = runtime
            .register_wait_for(
                "default",
                work_item_id.clone(),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "timer admission race".into(),
                None,
            )
            .await
            .unwrap();
        runtime.fire_timer_record(&mut timer).await.unwrap();
        let wake = runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .unwrap();
        if admitted {
            assert!(matches!(
                scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                    .poll()
                    .await
                    .unwrap(),
                scheduler_executor::RunLoopPoll::Message(_)
            ));
        }
        let before = runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|wait| wait.id == registered.condition.id)
            .unwrap();
        let before_execution = work_item_id
            .as_ref()
            .map(|_| timer_execution_snapshot(&runtime));
        runtime.cancel_timer(&timer.id).await.unwrap();
        let after = runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|wait| wait.id == before.id)
            .unwrap();
        let queue = runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&wake.message_id)
            .unwrap()
            .unwrap();
        if admitted {
            assert_eq!(after, before, "cancellation must not retract admitted work");
            assert_eq!(queue.status, QueueEntryStatus::Dequeued);
        } else {
            assert_eq!(before.status, WaitConditionStatus::Triggered);
            assert_eq!(after.status, WaitConditionStatus::Cancelled);
            assert_eq!(after.trigger_message_id, before.trigger_message_id);
            assert_eq!(after.triggered_at, before.triggered_at);
            assert_eq!(queue.status, QueueEntryStatus::Dropped);
            assert!(matches!(
                scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                    .poll()
                    .await
                    .unwrap(),
                scheduler_executor::RunLoopPoll::Idle
            ));
        }
        if let Some(work_id) = work_item_id {
            let execution = timer_execution_snapshot(&runtime);
            let before_execution = before_execution.unwrap();
            assert_eq!(
                execution.work_items[&work_id].state,
                before_execution.work_items[&work_id].state
            );
            assert_eq!(execution.attempts, before_execution.attempts);
            assert_eq!(
                execution.work_items[&work_id].source_revision,
                runtime
                    .storage()
                    .latest_work_item(&work_id)
                    .unwrap()
                    .unwrap()
                    .revision
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_wait_write_failure_rolls_back_every_domain() {
    for failed_write in [
        "UPDATE ON wait_conditions WHEN NEW.status = 'cancelled'",
        "UPDATE ON work_items",
        "UPDATE ON execution_protocol_work_items",
        "INSERT ON execution_protocol_command_results",
        "INSERT ON audit_events WHEN NEW.kind = 'work_item_written'",
        "INSERT ON runtime_index_outbox",
    ] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("must not run")),
            "default".into(),
            context_config(),
            controlled_clock(),
        )
        .unwrap();
        let work = runtime
            .create_work_item("atomic cancel".into(), None, None, Vec::new())
            .await
            .unwrap();
        let mut timer = runtime
            .schedule_timer(60_000, Some(60_000), None)
            .await
            .unwrap();
        runtime
            .register_wait_for(
                "default",
                Some(work.id.clone()),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "cancel rollback".into(),
                Some(120_000),
            )
            .await
            .unwrap();
        runtime.fire_timer_record(&mut timer).await.unwrap();
        let before_waits = runtime.storage().latest_wait_conditions().unwrap();
        let before_work = runtime.storage().latest_work_item(&work.id).unwrap();
        let before_state = runtime.agent_state().await.unwrap();
        let before_events = runtime.storage().read_recent_events(100).unwrap();
        let before_execution = timer_execution_snapshot(&runtime);
        let wake = runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .unwrap();
        runtime
            .inner
            .runtime_db
            .connection()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER reject_timer_wait_cancel BEFORE {failed_write}
             BEGIN SELECT RAISE(ABORT, 'injected wait cancellation failure'); END;"
            ))
            .unwrap();
        assert!(runtime
            .cancel_timer(&timer.id)
            .await
            .unwrap_err()
            .to_string()
            .contains("injected wait cancellation failure"));
        assert_eq!(
            runtime.latest_timer(&timer.id).await.unwrap().unwrap(),
            timer
        );
        assert_eq!(
            runtime.storage().latest_wait_conditions().unwrap(),
            before_waits
        );
        assert_eq!(
            runtime.storage().latest_work_item(&work.id).unwrap(),
            before_work
        );
        assert_eq!(runtime.agent_state().await.unwrap(), before_state);
        assert_eq!(timer_execution_snapshot(&runtime), before_execution);
        assert_eq!(
            runtime.storage().read_recent_events(100).unwrap(),
            before_events
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .timers()
                .pending_wake(&timer.id)
                .unwrap(),
            Some(wake.clone())
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&wake.message_id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
        runtime
            .inner
            .runtime_db
            .connection()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_timer_wait_cancel;")
            .unwrap();
        runtime.cancel_timer(&timer.id).await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_timer_legacy_orphan_recovery_and_repeat_cancel_are_idempotent() {
    for restart in [false, true] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let clock = controlled_clock();
        let make_runtime = |home: &std::path::Path| {
            RuntimeHandle::new_with_clock(
                "default",
                home.to_path_buf(),
                workspace.path().to_path_buf(),
                "http://127.0.0.1:7878".into(),
                Arc::new(StubProvider::new("must not run")),
                "default".into(),
                context_config(),
                clock.clone(),
            )
            .unwrap()
        };
        let original_home = tempdir().unwrap();
        let runtime = make_runtime(original_home.path());
        let work = runtime
            .create_work_item("legacy orphan".into(), None, None, Vec::new())
            .await
            .unwrap();
        let mut timer = runtime.schedule_timer(60_000, None, None).await.unwrap();
        let registered = runtime
            .register_wait_for(
                "default",
                Some(work.id.clone()),
                WaitForWakeKind::Timer,
                Some(timer.id.clone()),
                "legacy cancelled dependency".into(),
                Some(120_000),
            )
            .await
            .unwrap();
        // Import a real pre-cutover orphan, not an out-of-band current-schema write.
        let blocked = runtime.latest_work_item(&work.id).await.unwrap().unwrap();
        let state = runtime.agent_state().await.unwrap();
        timer.status = TimerStatus::Cancelled;
        timer.next_fire_at = None;
        seed_legacy_timer_backlog_with_wait(
            dir.path(),
            std::slice::from_ref(&timer),
            &[],
            Some((&blocked, &registered.condition, &state)),
        );
        drop(runtime);
        clock.rewind(std::time::Duration::from_secs(1));
        let runtime = make_runtime(dir.path());
        let runtime = if restart {
            make_runtime(dir.path())
        } else {
            runtime
        };
        runtime.cancel_timer(&timer.id).await.unwrap();
        let waits = runtime.storage().latest_wait_conditions().unwrap();
        let wait = waits
            .iter()
            .find(|wait| wait.id == registered.condition.id)
            .unwrap();
        assert_eq!(wait.status, WaitConditionStatus::Cancelled);
        assert_eq!(wait.cancelled_at, Some(clock.now()));
        assert_eq!(wait.updated_at, registered.condition.updated_at);
        let work = runtime.latest_work_item(&work.id).await.unwrap().unwrap();
        assert!(work
            .blocked_by
            .as_deref()
            .unwrap()
            .contains("timer_cancelled"));
        assert!(work.blocked_by_wait_id.is_none());
        assert!(work.recheck_at.is_none());
        let events = runtime.storage().read_recent_events(100).unwrap();
        runtime.cancel_timer(&timer.id).await.unwrap();
        let recovered_again = make_runtime(dir.path());
        assert_eq!(
            recovered_again.storage().latest_wait_conditions().unwrap(),
            waits
        );
        assert_eq!(
            recovered_again.storage().read_recent_events(100).unwrap(),
            events
        );
        assert_eq!(recovered_again.agent_state().await.unwrap().pending, 0);
    }
}

async fn assert_timer_fire_count(
    runtime: &RuntimeHandle,
    timer_id: &str,
    expected: u64,
) -> TimerRecord {
    for _ in 0..100 {
        let timers = runtime.recent_timers(100).await.unwrap();
        let timer = timers.iter().find(|timer| timer.id == timer_id).unwrap();
        if timer.fire_count == expected {
            return (**timer).clone();
        }
        tokio::task::yield_now().await;
    }
    panic!("timer {timer_id} did not reach fire_count {expected}");
}

fn assert_single_durable_timer_wake(runtime: &RuntimeHandle, timer_id: &str) -> String {
    let messages = runtime
        .storage()
        .read_recent_messages(100)
        .unwrap()
        .into_iter()
        .filter(|message| {
            matches!(&message.origin, MessageOrigin::Timer { timer_id: id } if id == timer_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(timer_id)
        .unwrap()
        .expect("timer must retain a durable pending wake");
    assert_eq!(wake.message_id, messages[0].id);
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&wake.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    wake.message_id
}

#[tokio::test(start_paused = true)]
async fn one_shot_timer_rechecks_wall_clock_after_long_suspend() {
    for hours in [24, 48] {
        let dir = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let clock = controlled_clock();
        let runtime = RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("timer done")),
            "default".into(),
            context_config(),
            clock.clone(),
        )
        .unwrap();
        let timer = runtime.schedule_timer(3_600_000, None, None).await.unwrap();
        yield_timer_loop().await;
        assert_timer_fire_count(&runtime, &timer.id, 0).await;

        clock.advance(std::time::Duration::from_secs(hours * 3_600));
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        yield_timer_loop().await;
        let fired = assert_timer_fire_count(&runtime, &timer.id, 1).await;
        assert_eq!(fired.status, TimerStatus::Completed);
        assert_single_durable_timer_wake(&runtime, &timer.id);
        assert_eq!(runtime.agent_state().await.unwrap().pending, 1);

        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        yield_timer_loop().await;
        assert_timer_fire_count(&runtime, &timer.id, 1).await;
        assert_single_durable_timer_wake(&runtime, &timer.id);
    }
}

#[tokio::test(start_paused = true)]
async fn repeating_timer_rechecks_wall_clock_on_each_arm_without_replaying_history() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let timer = runtime
        .schedule_timer(3_600_000, Some(7 * 3_600_000), None)
        .await
        .unwrap();
    let original_deadline = timer.next_fire_at.unwrap();
    yield_timer_loop().await;
    assert_timer_fire_count(&runtime, &timer.id, 0).await;

    let mut first_wake = None;
    for (round, hours) in [24, 48].into_iter().enumerate() {
        clock.advance(std::time::Duration::from_secs(hours * 3_600));
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        yield_timer_loop().await;
        let fired = assert_timer_fire_count(&runtime, &timer.id, (round + 1) as u64).await;
        assert_eq!(fired.status, TimerStatus::Active);
        let next = fired.next_fire_at.unwrap();
        assert!(next > clock.now());
        assert!(next <= clock.now() + chrono::Duration::hours(7));
        assert_eq!(
            (next - original_deadline).num_milliseconds() % (7 * 3_600_000),
            0
        );
        let wake_id = assert_single_durable_timer_wake(&runtime, &timer.id);
        if let Some(first) = &first_wake {
            assert_eq!(&wake_id, first);
        } else {
            first_wake = Some(wake_id);
        }
        assert_eq!(runtime.agent_state().await.unwrap().pending, 1);
        // Let the next sleep arm before separating the clocks again.
        yield_timer_loop().await;
        assert_timer_fire_count(&runtime, &timer.id, (round + 1) as u64).await;
    }
    runtime.cancel_timer(&timer.id).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn suppressed_timer_fire_emits_observable_audit_event() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let timer = runtime
        .schedule_timer(3_600_000, Some(7 * 3_600_000), None)
        .await
        .unwrap();
    yield_timer_loop().await;

    // First fire creates the durable pending wake.
    clock.advance(std::time::Duration::from_secs(24 * 3_600));
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    yield_timer_loop().await;
    assert_timer_fire_count(&runtime, &timer.id, 1).await;
    let wake_id = assert_single_durable_timer_wake(&runtime, &timer.id);

    // Second fire is suppressed by the one-pending-wake guard and must be
    // observable instead of silently advancing fire_count (#3607).
    clock.advance(std::time::Duration::from_secs(8 * 3_600));
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    yield_timer_loop().await;
    assert_timer_fire_count(&runtime, &timer.id, 2).await;
    assert_eq!(
        assert_single_durable_timer_wake(&runtime, &timer.id),
        wake_id
    );

    let suppressed = runtime
        .storage()
        .read_recent_events(64)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "timer_wake_suppressed")
        .expect("suppressed timer fire must emit an audit event");
    assert_eq!(suppressed.data["timer_id"], serde_json::json!(timer.id));
    assert_eq!(suppressed.data["fire_count"], serde_json::json!(2));
    assert_eq!(
        suppressed.data["pending_message_id"],
        serde_json::json!(wake_id)
    );
    assert!(suppressed.data["pending_age_ms"].as_i64().unwrap() > 0);
    runtime.cancel_timer(&timer.id).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_sleeping_timer_cannot_fire_after_wall_clock_suspend() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let timer = runtime
        .schedule_timer(3_600_000, Some(3_600_000), None)
        .await
        .unwrap();
    yield_timer_loop().await;
    runtime.cancel_timer(&timer.id).await.unwrap();
    clock.advance(std::time::Duration::from_secs(48 * 3_600));
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    yield_timer_loop().await;
    let cancelled = assert_timer_fire_count(&runtime, &timer.id, 0).await;
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
    assert!(!runtime.storage().read_recent_messages(100).unwrap().iter().any(
        |message| matches!(&message.origin, MessageOrigin::Timer { timer_id } if timer_id == &timer.id)
    ));
    assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
}

#[tokio::test(start_paused = true)]
async fn runtime_fires_overdue_timer_after_restart() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    seed_legacy_timer_backlog(
        dir.path(),
        &[TimerRecord {
            id: "timer-recover".into(),
            agent_id: "default".into(),
            created_at: now - chrono::Duration::milliseconds(10),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("timer recovered".into()),
            next_fire_at: Some(now - chrono::Duration::milliseconds(5)),
            last_fired_at: None,
            fire_count: 0,
            calendar: None,
        }],
        &[],
    );

    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        100,
        |events| events.iter().any(|event| event.kind == "timer_fired"),
        "recovered overdue timer",
    )
    .await;

    let timers = runtime.recent_timers(10).await.unwrap();
    let timer = timers
        .iter()
        .find(|timer| timer.id == "timer-recover" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime_task.abort();
}

#[tokio::test(start_paused = true)]
async fn runtime_recovers_active_timer_without_next_fire_at() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    seed_legacy_timer_backlog(
        dir.path(),
        &[TimerRecord {
            id: "timer-missing-next-fire".into(),
            agent_id: "default".into(),
            created_at: now - chrono::Duration::milliseconds(20),
            duration_ms: 10,
            interval_ms: None,
            repeat: false,
            status: TimerStatus::Active,
            summary: Some("timer fallback".into()),
            next_fire_at: None,
            last_fired_at: None,
            fire_count: 0,
            calendar: None,
        }],
        &[],
    );

    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer fallback done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        100,
        |events| events.iter().any(|event| event.kind == "timer_fired"),
        "recovered timer without next_fire_at",
    )
    .await;

    let timers = runtime.recent_timers(10).await.unwrap();
    let timer = timers
        .iter()
        .find(|timer| timer.id == "timer-missing-next-fire" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime_task.abort();
}

#[tokio::test(start_paused = true)]
async fn runtime_recovers_overdue_repeating_timer_on_original_schedule() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    let scheduled_fire_at = now - chrono::Duration::seconds(5);
    seed_legacy_timer_backlog(
        dir.path(),
        &[TimerRecord {
            id: "timer-recover-repeat".into(),
            agent_id: "default".into(),
            created_at: scheduled_fire_at - chrono::Duration::seconds(1),
            duration_ms: 1_000,
            interval_ms: Some(1_000),
            repeat: true,
            status: TimerStatus::Active,
            summary: Some("repeating timer recovered".into()),
            next_fire_at: Some(scheduled_fire_at),
            last_fired_at: None,
            fire_count: 0,
            calendar: None,
        }],
        &[],
    );

    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        100,
        |events| {
            events.iter().any(|event| {
                event.kind == "timer_fired"
                    && event
                        .data
                        .get("timer_id")
                        .and_then(serde_json::Value::as_str)
                        == Some("timer-recover-repeat")
            })
        },
        "recovered overdue repeating timer",
    )
    .await;

    let timers = runtime.recent_timers(10).await.unwrap();
    let timer = timers
        .iter()
        .find(|timer| timer.id == "timer-recover-repeat" && timer.fire_count == 1)
        .unwrap();
    assert_eq!(timer.status, TimerStatus::Active);
    assert_eq!(timer.next_fire_at, Some(now + chrono::Duration::seconds(1)));
    runtime_task.abort();
}

#[tokio::test]
async fn schedule_timer_rejects_unrepresentable_duration() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let result = runtime.schedule_timer(u64::MAX, None, None).await;
    assert!(result.is_err());
}

#[tokio::test(start_paused = true)]
async fn calendar_timer_persists_rule_and_next_occurrence() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("calendar timer")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();

    let timer = runtime
        .schedule_calendar_timer("* * * * *", "UTC", Some("calendar reminder".into()))
        .await
        .unwrap();

    assert_eq!(
        timer.calendar.as_ref().map(|calendar| &calendar.expression),
        Some(&"0 * * * * *".to_string())
    );
    assert_eq!(
        timer
            .calendar
            .as_ref()
            .map(|calendar| calendar.timezone.as_str()),
        Some("UTC")
    );
    assert!(timer.repeat);
    assert!(timer
        .next_fire_at
        .is_some_and(|next| next > timer.created_at));
    assert_eq!(
        runtime.storage().latest_timer_record(&timer.id).unwrap(),
        Some(timer)
    );
}

#[tokio::test(start_paused = true)]
async fn timer_message_binds_the_unique_matching_wait_work_item() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock.clone(),
    )
    .unwrap();
    let work = runtime
        .create_work_item("wait for timer".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let timer = runtime
        .schedule_timer(100, None, Some("bound timer".into()))
        .await
        .unwrap();
    let registration = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "waiting for bound timer".into(),
            None,
        )
        .await
        .unwrap();

    clock.advance(std::time::Duration::from_millis(100));
    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    let message = runtime
        .storage()
        .read_recent_messages(10)
        .unwrap()
        .into_iter()
        .find(|message| message.kind == MessageKind::TimerTick)
        .expect("timer tick should be queued");
    assert_eq!(message.work_item_id.as_deref(), Some(work.id.as_str()));
    assert_eq!(message.source_refs.get("timer_id"), Some(&timer.id));
    let triggered = runtime
        .storage()
        .latest_wait_conditions()
        .unwrap()
        .into_iter()
        .find(|condition| condition.id == registration.condition.id)
        .expect("timer wait should remain durable");
    assert_eq!(triggered.status, WaitConditionStatus::Triggered);
    assert_eq!(
        triggered.trigger_message_id(),
        Some(message.id.as_str()),
        "timer fire must trigger the exact wait in the enqueue transaction"
    );
}

#[tokio::test(start_paused = true)]
async fn timer_fire_fault_rolls_back_the_wait_and_wake_transaction() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let work = runtime
        .create_work_item("atomic timer wake".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let mut timer = runtime
        .schedule_timer(10_000, None, Some("atomic timer".into()))
        .await
        .unwrap();
    let registration = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "waiting for atomic timer".into(),
            None,
        )
        .await
        .unwrap();

    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::AfterCanonicalWrites,
    );
    let error = runtime.fire_timer_record(&mut timer).await.unwrap_err();
    assert_injected_transition_fault(&error);
    let timers = runtime.recent_timers(10).await.unwrap();
    let persisted = timers.iter().find(|record| record.id == timer.id).unwrap();
    assert_eq!(persisted.status, TimerStatus::Active);
    assert_eq!(persisted.fire_count, 0);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
    assert!(!runtime
        .storage()
        .read_recent_messages(10)
        .unwrap()
        .iter()
        .any(|message| message.kind == MessageKind::TimerTick));
    assert_eq!(
        runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|condition| condition.id == registration.condition.id)
            .map(|condition| condition.status),
        Some(WaitConditionStatus::Active)
    );
    assert_eq!(runtime.agent_state().await.unwrap().pending, 0);

    runtime.fire_timer_record(&mut timer).await.unwrap();
    assert_eq!(
        runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|condition| condition.id == registration.condition.id)
            .map(|condition| condition.status),
        Some(WaitConditionStatus::Triggered)
    );
}

#[tokio::test(start_paused = true)]
async fn repeating_timer_coalesces_to_one_pending_wake() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let mut timer = runtime
        .schedule_timer(10_000, Some(10_000), Some("coalesced timer".into()))
        .await
        .unwrap();

    for _ in 0..25 {
        runtime.fire_timer_record(&mut timer).await.unwrap();
    }

    let timers = runtime.recent_timers(10).await.unwrap();
    let persisted = timers.iter().find(|record| record.id == timer.id).unwrap();
    assert_eq!(persisted.fire_count, 25);
    let messages = runtime
        .storage()
        .read_recent_messages(100)
        .unwrap()
        .into_iter()
        .filter(|message| {
            matches!(
                &message.origin,
                MessageOrigin::Timer { timer_id } if timer_id == &timer.id
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake.message_id, messages[0].id);
    assert_eq!(wake.fire_count, 1);
    assert_eq!(runtime.agent_state().await.unwrap().pending, 1);
}

#[tokio::test(start_paused = true)]
async fn cancelling_timer_drops_pending_wake_and_stale_fire_cannot_revive_it() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("timer done")),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    let mut stale_timer = runtime
        .schedule_timer(10_000, Some(10_000), Some("cancel timer".into()))
        .await
        .unwrap();
    runtime.fire_timer_record(&mut stale_timer).await.unwrap();
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&stale_timer.id)
        .unwrap()
        .unwrap();

    let cancelled = runtime.cancel_timer(&stale_timer.id).await.unwrap();
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&stale_timer.id)
        .unwrap()
        .is_none());
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&wake.message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Dropped
    );
    assert_eq!(runtime.agent_state().await.unwrap().pending, 0);

    runtime.fire_timer_record(&mut stale_timer).await.unwrap();
    let timers = runtime.recent_timers(10).await.unwrap();
    let persisted = timers
        .iter()
        .find(|record| record.id == stale_timer.id)
        .unwrap();
    assert_eq!(persisted.status, TimerStatus::Cancelled);
    assert_eq!(persisted.fire_count, 1);
}

#[tokio::test(start_paused = true)]
async fn completed_one_shot_wait_binds_existing_pending_wake() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
        reply: "timer handled",
    });
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
        clock,
    )
    .unwrap();
    // Timer wake settlement must not depend on the host user's skill catalogue.
    let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
    config.user_home_dir = None;
    runtime.inner.config_snapshot.store(Arc::new(config));
    let work = runtime
        .create_work_item("late timer wait".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime.pick_work_item(work.id.clone()).await.unwrap();
    let mut work_message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "run before late timer wait".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    bind_autonomous_work_queue_tick(&mut work_message, &work, "continue_active");
    work_message.turn_id = Some("turn-late-one-shot-wait".into());
    let work_message = runtime.enqueue(work_message).await.unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("initial timer Work dispatch must be claimed");
    };
    assert_eq!(scheduled.message.id, work_message.id);
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
        .await
        .unwrap();

    let mut timer = runtime
        .schedule_timer(10_000, None, Some("one shot".into()))
        .await
        .unwrap();
    runtime.fire_timer_record(&mut timer).await.unwrap();
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();

    let registration = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "wait registered after fire".into(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        registration.condition.status,
        WaitConditionStatus::Triggered
    );
    assert_eq!(
        registration.condition.trigger_message_id(),
        Some(wake.message_id.as_str())
    );

    let mut terminal = terminal_transition(&work_message, Some(&work.id));
    terminal.turn_record = runtime.build_turn_record(&terminal.terminal).await.unwrap();
    runtime
        .commit_queue_terminal_settlement(
            QueueEntryRecord {
                message_id: work_message.id.clone(),
                agent_id: work_message.agent_id.clone(),
                priority: work_message.priority,
                status: QueueEntryStatus::Processed,
                created_at: work_message.created_at,
                updated_at: Utc::now(),
            },
            Vec::new(),
            true,
            Some(&terminal),
        )
        .await
        .unwrap();
    finish_claimed_test_run(&runtime).await;
    drop(scheduled);
    let runtime_task = tokio::spawn(runtime.clone().run());
    wait_for_audit_events(
        &runtime,
        200,
        |events| {
            events.iter().any(|event| {
                event.kind == "queue_entry_settled"
                    && event.data["message_id"] == wake.message_id.as_str()
            })
        },
        "late one-shot timer wait settlement",
    )
    .await;
    runtime_task.abort();

    assert_eq!(provider.call_count().await, 1);
    let condition = runtime
        .storage()
        .latest_wait_conditions()
        .unwrap()
        .into_iter()
        .find(|condition| condition.id == registration.condition.id)
        .unwrap();
    assert_eq!(condition.status, WaitConditionStatus::Resolved);
    let work = runtime
        .inner
        .runtime_db
        .work_items()
        .latest(&work.id)
        .unwrap()
        .unwrap();
    assert!(work.blocked_by.is_none());
}

#[tokio::test(start_paused = true)]
async fn recovery_normalizes_legacy_timer_backlog_idempotently() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    let active_timer = TimerRecord {
        id: "legacy-active-timer".into(),
        agent_id: "default".into(),
        created_at: now,
        duration_ms: 10_000,
        interval_ms: Some(10_000),
        repeat: true,
        status: TimerStatus::Active,
        summary: Some("legacy active".into()),
        next_fire_at: Some(now + chrono::Duration::seconds(10)),
        last_fired_at: Some(now),
        fire_count: 3,
        calendar: None,
    };
    let cancelled_timer = TimerRecord {
        id: "legacy-cancelled-timer".into(),
        status: TimerStatus::Cancelled,
        next_fire_at: None,
        summary: Some("legacy cancelled".into()),
        ..active_timer.clone()
    };

    let mut messages = Vec::new();
    let mut active_message_ids = Vec::new();
    let mut cancelled_message_ids = Vec::new();
    for (timer, ids) in [
        (&active_timer, &mut active_message_ids),
        (&cancelled_timer, &mut cancelled_message_ids),
    ] {
        for _ in 0..3 {
            let message = MessageEnvelope {
                metadata: Some(serde_json::json!({ "timer_id": timer.id })),
                ..MessageEnvelope::new(
                    "default",
                    MessageKind::TimerTick,
                    MessageOrigin::Timer {
                        timer_id: timer.id.clone(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Next,
                    MessageBody::Text {
                        text: "legacy timer tick".into(),
                    },
                )
                .with_admission(
                    MessageDeliverySurface::TimerScheduler,
                    AdmissionContext::RuntimeOwned,
                )
            };
            ids.push(message.id.clone());
            messages.push(message);
        }
    }

    seed_legacy_timer_backlog(
        dir.path(),
        &[active_timer.clone(), cancelled_timer.clone()],
        &messages,
    );

    let make_runtime = || {
        RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("timer done")),
            "default".into(),
            context_config(),
            clock.clone(),
        )
        .unwrap()
    };
    let runtime = make_runtime();
    let active_statuses = active_message_ids
        .iter()
        .map(|message_id| {
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(message_id)
                .unwrap()
                .unwrap()
                .status
        })
        .collect::<Vec<_>>();
    assert_eq!(
        active_statuses
            .iter()
            .filter(|status| **status == QueueEntryStatus::Queued)
            .count(),
        1
    );
    assert_eq!(
        active_statuses
            .iter()
            .filter(|status| **status == QueueEntryStatus::Dropped)
            .count(),
        2
    );
    assert!(cancelled_message_ids.iter().all(|message_id| {
        runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(message_id)
            .unwrap()
            .unwrap()
            .status
            == QueueEntryStatus::Dropped
    }));
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&active_timer.id)
        .unwrap()
        .unwrap();
    assert!(active_message_ids.contains(&wake.message_id));
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&cancelled_timer.id)
        .unwrap()
        .is_none());
    assert_eq!(runtime.agent_state().await.unwrap().pending, 1);

    let recovered_again = make_runtime();
    let wake_again = recovered_again
        .inner
        .runtime_db
        .timers()
        .pending_wake(&active_timer.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake_again.message_id, wake.message_id);
    assert_eq!(recovered_again.agent_state().await.unwrap().pending, 1);
}

#[tokio::test(start_paused = true)]
async fn recovery_reactivates_incorporated_wake_for_restored_timer_message() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let clock = controlled_clock();
    let now = clock.now();
    let timer = TimerRecord {
        id: "timer-restored".into(),
        agent_id: "default".into(),
        created_at: now,
        duration_ms: 10_000,
        interval_ms: Some(10_000),
        repeat: true,
        status: TimerStatus::Active,
        summary: Some("restored timer".into()),
        next_fire_at: Some(now + chrono::Duration::seconds(10)),
        last_fired_at: Some(now),
        fire_count: 1,
        calendar: None,
    };
    let message = MessageEnvelope {
        metadata: Some(serde_json::json!({ "timer_id": timer.id })),
        ..MessageEnvelope::new(
            "default",
            MessageKind::TimerTick,
            MessageOrigin::Timer {
                timer_id: timer.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "restored timer tick".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TimerScheduler,
            AdmissionContext::RuntimeOwned,
        )
    };
    seed_legacy_timer_backlog(dir.path(), &[timer.clone()], &[message.clone()]);

    let make_runtime = || {
        RuntimeHandle::new_with_clock(
            "default",
            dir.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("timer done")),
            "default".into(),
            context_config(),
            clock.clone(),
        )
        .unwrap()
    };

    // First bootstrap records the pending wake for the queued timer message.
    let runtime = make_runtime();
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake.message_id, message.id);

    // Use the actual Queue claim/whole ACK; a raw Wake status update cannot
    // stand in for its scheduling alias and original pending disposition.
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("original Timer source must be claimed")
    };
    assert_eq!(scheduled.message.id, message.id);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .connection()
            .unwrap()
            .query_row(
                "SELECT status FROM timer_wakes WHERE timer_id=?1 AND message_id=?2",
                rusqlite::params![timer.id, message.id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "incorporated"
    );
    drop(scheduled);
    // End the original instance before the exclusive pre-activation actor.
    // The scoped actor updates the actual Attempt/Outcome and its D3 source
    // projection together; the old cfg-only unscoped repair cannot do that.
    let db = runtime.inner.runtime_db.clone();
    let ledger = runtime.inner.input_evidence_budget.clone();
    runtime
        .retire_instance_locally("fixture_daemon_restart")
        .await;
    runtime.join_retiring_resources().await.unwrap();
    drop(runtime);
    let repaired = {
        let path = dir.path().join(".holon/state/runtime-maintenance.lock");
        let lease = crate::runtime_db::RuntimeDbLock::try_lock(&path).unwrap();
        let handoff = tokio::sync::Mutex::new(());
        let held = handoff.try_lock().unwrap();
        let capability = crate::runtime_db::input_evidence_snapshot::legacy_recovery::ExclusiveStartupInputRecovery::for_pre_activation_host(
            &db, &lease, &path, &held,
        ).unwrap();
        let scopes = db
            .startup_input_recovery_scopes(&["default".into()])
            .unwrap();
        db.recover_interrupted_runtime_state_at_startup_with_tasks_in_scopes(
            &scopes,
            &capability,
            &ledger,
        )
        .unwrap()
    };
    assert!(repaired
        .report
        .recovered_queue_agent_ids
        .iter()
        .any(|id| id == "default"));
    assert_eq!(repaired.report.interrupted_execution_attempts, 1);
    assert_eq!(
        db.queue_entries()
            .latest(&message.id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Interrupted
    );

    // Recovery must be idempotent instead of failing the whole bootstrap with a
    // timer_wakes primary-key conflict.
    let recovered = make_runtime();
    let wake = recovered
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake.message_id, message.id);
    assert_eq!(recovered.agent_state().await.unwrap().pending, 1);
}

async fn terminal_timer_fixture(
    repeat: bool,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    RuntimeHandle,
    TimerRecord,
) {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new_with_clock(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("must not run")),
        "default".into(),
        context_config(),
        controlled_clock(),
    )
    .unwrap();
    let mut timer = runtime
        .schedule_timer(60_000, repeat.then_some(60_000), None)
        .await
        .unwrap();
    runtime.fire_timer_record(&mut timer).await.unwrap();
    (dir, workspace, runtime, timer)
}

fn terminal_timer_queue_command(
    runtime: &RuntimeHandle,
    timer: &TimerRecord,
) -> crate::runtime_db::transitions::QueueTransitionCommand {
    use crate::runtime_db::transitions::{QueueMutation, QueueOperation, QueueTransitionCommand};
    let wake = runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .unwrap();
    let expected = runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest(&wake.message_id)
        .unwrap()
        .unwrap();
    let mut record = expected.clone();
    record.status = QueueEntryStatus::Dropped;
    record.updated_at = runtime.now().max(record.updated_at);
    QueueTransitionCommand {
        agent_id: "default".into(),
        operation: QueueOperation::Settle,
        mutation: QueueMutation::CompareAndSet { expected, record },
        scheduler_claim_work_item: None,
        agent_state: None,
        message_evidence: Vec::new(),
        transcript_entries: Vec::new(),
        turn_record: None,
        audit_events: Vec::new(),
        notify_scheduler: false,
        fault: None,
        brief_evidence: Vec::new(),
    }
}

fn timer_retirement_events(runtime: &RuntimeHandle) -> Vec<AuditEvent> {
    runtime
        .storage()
        .read_recent_events(100)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "timer_wake_terminal_retired")
        .collect()
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_recovery_never_reactivates_cancelled_wake() {
    let (_dir, _workspace, runtime, mut timer) = terminal_timer_fixture(true).await;
    let entry = legacy_timer_queue_status(&runtime, &timer, QueueEntryStatus::Queued);
    runtime.cancel_timer(&timer.id).await.unwrap();
    // Old interrupted-source restoration must not undo a cancellation fence.
    runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute(
            "UPDATE queue_entries SET status = 'queued', payload_json = ?2
             WHERE message_id = ?1",
            rusqlite::params![entry.message_id, serde_json::to_string(&entry).unwrap()],
        )
        .unwrap();
    let result = runtime
        .inner
        .runtime_db
        .timers()
        .normalize_wakes_for_recovery("default", runtime.now())
        .unwrap();
    assert_eq!(result.reactivated_wakes, 0);
    assert_eq!(result.dropped_message_ids, vec![entry.message_id]);
    runtime.fire_timer_record(&mut timer).await.unwrap();
    assert_eq!(
        runtime
            .inner
            .runtime_db
            .timers()
            .latest(&timer.id)
            .unwrap()
            .unwrap()
            .status,
        TimerStatus::Cancelled
    );
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_retirement_preserves_wait_and_business_blocker() {
    let (_dir, _workspace, runtime, timer) = terminal_timer_fixture(true).await;
    let work = runtime
        .create_work_item("independent timer blocker".into(), None, None, Vec::new())
        .await
        .unwrap();
    let registration = runtime
        .register_wait_for(
            "default",
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "timer continuation".into(),
            None,
        )
        .await
        .unwrap();
    let before = runtime
        .storage()
        .latest_work_item(&work.id)
        .unwrap()
        .unwrap();
    runtime
        .inner
        .runtime_db
        .transitions()
        .commit_queue(&terminal_timer_queue_command(&runtime, &timer))
        .unwrap();
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
    assert_eq!(
        runtime
            .storage()
            .latest_wait_conditions()
            .unwrap()
            .into_iter()
            .find(|wait| wait.id == registration.condition.id)
            .unwrap(),
        registration.condition
    );
    assert_eq!(
        runtime.storage().latest_work_item(&work.id).unwrap(),
        Some(before)
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_retirement_fences_exact_fire_and_never_refires_one_shot() {
    let (_dir, _workspace, runtime, mut timer) = terminal_timer_fixture(false).await;
    assert_eq!(timer.status, TimerStatus::Completed);
    runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute(
            "UPDATE timer_wakes SET fire_count = 2 WHERE timer_id = ?1",
            [&timer.id],
        )
        .unwrap();
    runtime
        .inner
        .runtime_db
        .transitions()
        .commit_queue(&terminal_timer_queue_command(&runtime, &timer))
        .unwrap();
    let mismatch = runtime
        .inner
        .runtime_db
        .timers()
        .normalize_wakes_for_recovery("default", runtime.now())
        .unwrap();
    assert_eq!(mismatch.invalidated_wakes, 0);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_some());
    runtime
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute(
            "UPDATE timer_wakes SET fire_count = 1 WHERE timer_id = ?1",
            [&timer.id],
        )
        .unwrap();
    let matched = runtime
        .inner
        .runtime_db
        .timers()
        .normalize_wakes_for_recovery("default", runtime.now())
        .unwrap();
    assert_eq!(matched.invalidated_wakes, 1);
    runtime.fire_timer_record(&mut timer).await.unwrap();
    assert_eq!(timer.fire_count, 1);
    assert_eq!(timer.status, TimerStatus::Completed);
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_recovery_preserves_unsettled_attempt_consumption() {
    use crate::domain::execution_protocol::*;
    for state in [
        ExecutionAttemptState::Open,
        ExecutionAttemptState::Interrupted,
        ExecutionAttemptState::ProtocolViolation,
    ] {
        let (_dir, _workspace, runtime, timer) = terminal_timer_fixture(true).await;
        let message_id = assert_single_durable_timer_wake(&runtime, &timer.id);
        let mut attempt = ExecutionAttempt {
            attempt_id: "retained-timer-attempt".into(),
            agent_id: "default".into(),
            source_message_id: None,
            input_projection: None,
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::QueueMessage {
                    message_id: message_id.clone(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::AgentLifecycle {
                agent_id: "default".into(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::Timer,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Next,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: 1,
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state,
            run_id: None,
            turn_id: None,
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: runtime.now().to_rfc3339(),
            terminal_at: None,
        };
        let mut protocol = ExecutionProtocolState::empty("default");
        if state != ExecutionAttemptState::Open {
            let outcome = ExecutionOutcomeRecord {
                outcome_id: "retained-timer-outcome".into(),
                attempt_id: attempt.attempt_id.clone(),
                outcome: ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                    reason: "retained timer consumption".into(),
                }),
                created_at: runtime.now().to_rfc3339(),
            };
            attempt.terminal_outcome_id = Some(outcome.outcome_id.clone());
            attempt.terminal_at = Some(outcome.created_at.clone());
            protocol
                .outcomes
                .insert(outcome.outcome_id.clone(), outcome);
        }
        protocol
            .attempts
            .insert(attempt.attempt_id.clone(), attempt);
        runtime
            .inner
            .runtime_db
            .transaction(|tx| {
                crate::runtime_db::transitions::execution_protocol_repository::persist_state_tx(
                    tx, &protocol,
                )
            })
            .unwrap();
        runtime
            .inner
            .runtime_db
            .transitions()
            .commit_queue(&terminal_timer_queue_command(&runtime, &timer))
            .unwrap();
        runtime
            .inner
            .runtime_db
            .timers()
            .normalize_wakes_for_recovery("default", runtime.now())
            .unwrap();
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .timers()
                .pending_wake(&timer.id)
                .unwrap()
                .unwrap()
                .message_id,
            message_id
        );
        assert!(timer_retirement_events(&runtime).is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_retirement_observes_same_transaction_turn_settlement() {
    let (_dir, _workspace, runtime, timer) = terminal_timer_fixture(true).await;
    let message_id = assert_single_durable_timer_wake(&runtime, &timer.id);
    let mut turn = TurnRecord::new("default", "settling-timer-turn", 1);
    turn.created_at = runtime.now();
    turn.input_message_ids = vec![message_id];
    runtime.storage().append_turn(&turn).unwrap();
    turn.terminal = Some(crate::types::TurnTerminalSummary {
        kind: crate::types::TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason: None,
        completed_at: runtime.now(),
        duration_ms: 0,
    });
    let mut command = terminal_timer_queue_command(&runtime, &timer);
    command.turn_record = Some(turn);
    runtime
        .inner
        .runtime_db
        .transitions()
        .commit_queue(&command)
        .unwrap();
    assert!(runtime
        .inner
        .runtime_db
        .timers()
        .pending_wake(&timer.id)
        .unwrap()
        .is_none());
    assert_eq!(timer_retirement_events(&runtime).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_drop_releases_pending_for_next_fire() {
    use crate::runtime_db::transitions::QueueMutation;
    for status in [QueueEntryStatus::Dropped, QueueEntryStatus::Quarantined] {
        let (_dir, _workspace, runtime, mut timer) = terminal_timer_fixture(true).await;
        let old_wake = assert_single_durable_timer_wake(&runtime, &timer.id);
        let mut command = terminal_timer_queue_command(&runtime, &timer);
        let QueueMutation::CompareAndSet { record, .. } = &mut command.mutation else {
            unreachable!();
        };
        record.status = status;
        assert!(
            runtime
                .inner
                .runtime_db
                .transitions()
                .commit_queue(&command)
                .unwrap()
                .applied
        );
        assert!(runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .is_none());
        let events = timer_retirement_events(&runtime);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data["message_id"], old_wake);
        assert_eq!(events[0].data["fire_count"], 1);
        let history: (String, Option<String>) = runtime
            .inner
            .runtime_db
            .connection()
            .unwrap()
            .query_row(
                "SELECT status, incorporated_at FROM timer_wakes WHERE message_id = ?1",
                [&old_wake],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(history, ("cancelled".into(), None));

        runtime.fire_timer_record(&mut timer).await.unwrap();
        let wake = runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .unwrap();
        assert_ne!(wake.message_id, old_wake);
        assert_eq!(wake.fire_count, 2);
        runtime.fire_timer_record(&mut timer).await.unwrap();
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .timers()
                .pending_wake(&timer.id)
                .unwrap()
                .unwrap(),
            wake,
            "duplicate ticks still coalesce after a terminal source is retired"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn terminal_timer_retirement_rolls_back_with_queue_transition() {
    use crate::runtime_db::transitions::{QueueMutation, TransitionFaultPoint};
    for fault in [
        TransitionFaultPoint::AfterCanonicalWrites,
        TransitionFaultPoint::AfterAuditWrites,
        TransitionFaultPoint::BeforeCommit,
    ] {
        let (_dir, _workspace, runtime, timer) = terminal_timer_fixture(true).await;
        let mut command = terminal_timer_queue_command(&runtime, &timer);
        let QueueMutation::CompareAndSet { expected, .. } = &command.mutation else {
            unreachable!();
        };
        let expected = expected.clone();
        command.fault = Some(fault);
        let error = runtime
            .inner
            .runtime_db
            .transitions()
            .commit_queue(&command)
            .unwrap_err();
        assert_injected_transition_fault(&error);
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&expected.message_id)
                .unwrap(),
            Some(expected)
        );
        assert!(runtime
            .inner
            .runtime_db
            .timers()
            .pending_wake(&timer.id)
            .unwrap()
            .is_some());
        assert!(timer_retirement_events(&runtime).is_empty());

        command.fault = None;
        runtime
            .inner
            .runtime_db
            .transitions()
            .commit_queue(&command)
            .unwrap();
        assert_eq!(timer_retirement_events(&runtime).len(), 1);
        // Either an OCC rejection or an idempotent result must leave history alone.
        let _ = runtime
            .inner
            .runtime_db
            .transitions()
            .commit_queue(&command);
        assert_eq!(timer_retirement_events(&runtime).len(), 1);
    }
}
