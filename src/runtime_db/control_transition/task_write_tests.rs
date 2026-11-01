use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
        resident_task::{
            CanonicalTaskFact, ResidentTaskDomain, TaskCaptureRequest, TaskFamily, TaskKey,
        },
        resident_work::WorkCaptureRequest,
    },
    runtime_db::{task_snapshot as tasks, work_snapshot::WorkDomainLoadRequest},
};
use std::collections::BTreeMap;

fn scope(f: &Fixture) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: f.key.agent_id.clone(),
        identity_incarnation: f.key.identity_incarnation,
    }
}
fn task_request() -> tasks::TaskDomainLoadRequest {
    tasks::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 8 * 1024 * 1024,
    }
}
fn work_request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 8 * 1024 * 1024,
    }
}
fn running(f: &Fixture, id: &str) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        agent_id: f.key.agent_id.clone(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: f.time,
        updated_at: f.time,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original admitted Task".into()),
        detail: Some(
            json!({"rejoin_obligation_id":id,"rejoin_generation":1,"parent_turn_id":"original-parent"}),
        ),
        recovery: None,
    }
}
fn seed_task(
    f: &Fixture,
    task: &TaskRecord,
    qualified: bool,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ResidentTaskDomain> {
    f.db.transaction(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &scope(f), &task_request(), ledger)?
            .require_current()?;
        if qualified {
            let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
            let proof = tasks::prepare_task_write_tx(tx, &scope(f), &keys)?;
            upsert_task_tx(tx, task)?;
            tasks::register_task_writes_with_budget_tx(
                tx,
                &proof,
                &keys,
                &f.db_state(tx)?,
                &task_request(),
                ledger,
            )?;
        } else {
            upsert_task_tx(tx, task)?;
        }
        Ok(tasks::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &scope(f),
            &task_request(),
            ledger,
        )?
        .require_current()?
        .resident)
    })
}
fn applied(
    f: &Fixture,
    command: &PreparedControl,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(ControlReceipt, Box<ControlCommittedDelta>)> {
    match f.db.commit_control_with_domain_requests(
        command,
        &work_request(),
        &task_request(),
        ledger,
    )? {
        ControlCommit::Applied { receipt, delta } => Ok((receipt, delta)),
        ControlCommit::Replayed { .. } => anyhow::bail!("first Control unexpectedly replayed"),
    }
}

#[test]
fn control_task_result_joins_one_final_task_revision_and_original_receipt() -> Result<()> {
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let old = seed_task(&f, &running(&f, "qualified-stop-task"), true, &ledger)?;
    let (input_owner, query_request) =
        crate::runtime_db::input_evidence_snapshot::prepare_complete_query_for_fixture(
            &f.db,
            &scope(&f),
            &ledger,
        )?;
    let old_input = input_owner.capture_for_request(&query_request)?;
    let query_source = input_owner
        .original_query_source()?
        .context("Control source")?;
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control_with_input_query(
        &command,
        &work_request(),
        &task_request(),
        &ledger,
        Some(&query_source),
        None,
    )?
    else {
        anyhow::bail!("first Control must apply");
    };
    let input = &delta.transition.effects.input_evidence_delta;
    input
        .query_advance
        .as_ref()
        .context("Control omitted its original Native Input advance")?
        .validate_delta(&scope(&f), &input.records)?;
    let current = delta
        .transition
        .effects
        .current_read_writes
        .as_ref()
        .context("Control omitted original Current ACK")?;
    current.validate_binding(&scope(&f), &ledger)?;
    assert_eq!(current.revision, current.base_revision + 1);
    assert!(current.qualifies(&(
        crate::runtime_db::current_read_writes::CurrentFamily::Queue,
        delta.result_messages[0].id.clone(),
    )));
    let birth: u64 = f.db.connection()?.query_row(
        "SELECT birth_seq FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message' AND record_id=?3",
        params![scope(&f).agent_id, scope(&f).identity_incarnation, delta.result_messages[0].id],
        |row| row.get(0),
    )?;
    assert_eq!(birth, 1);
    let next_input = input_owner
        .with_committed(input, Default::default())?
        .context("Control Input publication")?;
    let input_capture = next_input.capture_for_request(&query_request)?;
    let input_key = crate::runtime::resident_input_evidence::InputEvidenceKey::new(
        crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
        &delta.result_messages[0].id,
    );
    assert_eq!(input_capture.query.as_ref().unwrap().birth_cursor, birth);
    assert_eq!(
        input_capture.query.as_ref().unwrap().born,
        BTreeMap::from([(birth, input_key.clone())]),
    );
    assert!(input_capture.lookup(&input_key)?.is_some());
    assert_eq!(old_input.query.as_ref().unwrap().birth_cursor, 0);
    assert!(old_input.records.is_empty());
    let facts = delta
        .transition
        .effects
        .task_delta
        .as_ref()
        .context("Control omitted Task delta")?;
    assert_eq!(facts.base_revision, old.coverage().revision);
    assert_eq!(facts.revision, facts.base_revision + 1);
    assert!(facts.coverage.active_task_ids.is_empty());
    assert_eq!(facts.coverage.unsettled_result_ids.len(), 1);
    assert!(delta.transition.effects.task_delta_requires_prepared_domain);
    let current = old.with_committed(facts)?;
    let capture = current.capture(&TaskCaptureRequest::current())?;
    let key = TaskKey::new(TaskFamily::Task, "qualified-stop-task");
    let CanonicalTaskFact::Task(task) = &capture.lookup(&key)?.context("Task missing")?.fact else {
        anyhow::bail!("wrong family")
    };
    assert_eq!(task.status, TaskStatus::Interrupted);
    assert_eq!(
        old.capture(&TaskCaptureRequest::current())?
            .coverage
            .active_task_ids
            .len(),
        1
    );
    assert_eq!(delta.transition.effects.task_write_keys.len(), 2);
    assert!(delta
        .transition
        .effects
        .task_observation_keys
        .contains(&key));
    assert_eq!(delta.result_messages[0].message_seq, Some(1));
    let source = facts
        .coverage
        .output
        .heads
        .get("qualified-stop-task")
        .context("Control omitted final Task output head")?
        .source
        .as_ref()
        .context("Control omitted its actual Result source")?;
    assert_eq!(source.message.key.id, delta.result_messages[0].id);
    assert_eq!(source.message.scope, scope(&f));
    assert_eq!(source.message_status, TaskStatus::Interrupted);
    assert_eq!(
        facts
            .coverage
            .output
            .window
            .as_ref()
            .context("Control omitted its final received window")?
            .message_ids,
        vec![delta.result_messages[0].id.clone()]
    );
    assert_eq!(delta.queue_entries[0].status, QueueEntryStatus::Aborted);
    assert!(
        matches!(f.db.commit_control_with_domain_requests(&command, &work_request(), &task_request(), &ledger)?,
        ControlCommit::Replayed { receipt: original } if original == receipt)
    );
    let fresh = f.db.transaction(|tx| {
        Ok(
            tasks::load_task_domain_snapshot_tx(tx, &scope(&f), &task_request())?
                .require_current()?
                .resident,
        )
    })?;
    assert_eq!(fresh.coverage().revision, facts.revision);
    drop((capture, current, fresh, delta, old));
    drop((
        next_input,
        old_input,
        input_owner,
        query_source,
        query_request,
    ));
    assert!(
        ledger.report()?.observed_bytes > 0,
        "the last Input capture must keep its material charged",
    );
    drop(input_capture);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_generated_audit_ack_is_qualified_and_unrelated_prefix_is_not_adopted() -> Result<()> {
    use crate::runtime_db::current_read_writes::CurrentFamily;
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let task = running(&f, "generated-stop-task");
    seed_task(&f, &task, true, &ledger)?;
    let wait: WaitConditionRecord = serde_json::from_value(json!({
        "id": "wait-generated", "agent_id": f.key.agent_id, "status": "active",
        "kind": "task", "source": "task", "subject_ref": task.id,
        "waiting_for": "original TaskResult",
        "wake_sources": [{"kind": "task_result", "task_id": task.id}],
        "created_at": f.time, "updated_at": f.time, "turn_id": "original-parent",
    }))?;
    f.db.wait_conditions().upsert(&wait)?;
    let command = f.command(ControlAction::Stop);
    let result_id = &command.task_results[0].message.id;
    let actual_id = crate::runtime_db::transitions::wait_trigger_audit_id(result_id, None, false);
    let mut unrelated = AuditEvent::legacy("unrelated-prefix", json!({"raw": true}));
    unrelated.id = crate::runtime_db::transitions::wait_trigger_audit_id(result_id, None, true);
    f.db.transaction(|tx| {
        append_audit_event_tx(tx, Some(&f.key.agent_id), &unrelated)?;
        Ok(())
    })?;
    let ControlCommit::Applied { receipt, delta } = f.db.commit_control_with_input_query(
        &command,
        &work_request(),
        &task_request(),
        &ledger,
        None,
        None,
    )?
    else {
        anyhow::bail!("first Control must apply");
    };
    let current = delta
        .transition
        .effects
        .current_read_writes
        .as_ref()
        .context("original Current ACK")?;
    assert!(current.qualifies(&(CurrentFamily::Event, actual_id.clone())));
    assert!(!current
        .facts()
        .contains_key(&(CurrentFamily::Event, unrelated.id.clone())));
    let events = &delta.transition.effects.audit_events;
    let actual = events
        .iter()
        .find(|event| event.id == actual_id)
        .context("actual helper event")?;
    assert_eq!(actual.kind, "wait_condition_triggered");
    assert!(events.iter().all(|event| event.id != unrelated.id));
    assert!(receipt.audit_bindings.iter().any(
        |binding| binding.audit_event_id == actual_id && binding.event_seq == actual.event_seq
    ));
    assert!(receipt
        .audit_bindings
        .iter()
        .all(|binding| binding.audit_event_id != unrelated.id));
    assert!(matches!(
        f.db.commit_control_with_input_query(
            &command,
            &work_request(),
            &task_request(),
            &ledger,
            None,
            None,
        )?,
        ControlCommit::Replayed { .. }
    ));
    Ok(())
}

#[test]
fn control_raw_cleanup_preserves_business_without_adopting_task_or_new_result() -> Result<()> {
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let before = seed_task(&f, &running(&f, "raw-stop-task"), false, &ledger)?;
    let command = f.command(ControlAction::Stop);
    let (_, delta) = applied(&f, &command, &ledger)?;
    assert_eq!(delta.tasks[0].status, TaskStatus::Interrupted);
    assert_eq!(delta.result_messages.len(), 1);
    let facts = delta
        .transition
        .effects
        .task_delta
        .as_ref()
        .context("Task metadata omitted")?;
    assert!(facts.coverage.active_task_ids.is_empty());
    assert!(facts.coverage.unsettled_result_ids.is_empty());
    assert_eq!(delta.transition.effects.task_write_keys.len(), 2);
    let aliases: usize = f.db.transaction(|tx| {
        Ok(tx.query_row(
            "SELECT COUNT(*) FROM task_domain_records WHERE agent_id=?1 AND availability='present'",
            [&f.key.agent_id],
            |r| r.get(0),
        )?)
    })?;
    assert_eq!(
        aliases, 0,
        "old raw Task/derived raw Result must not gain qualification"
    );
    drop((before, delta));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_task_alias_fault_rolls_back_task_result_queue_state_and_receipt() -> Result<()> {
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let old = seed_task(&f, &running(&f, "rollback-task"), true, &ledger)?;
    let command = f.command(ControlAction::Stop);
    f.db.connection()?.execute_batch("CREATE TRIGGER fail_control_task_alias BEFORE INSERT ON task_domain_records WHEN NEW.record_id='rollback-task' BEGIN SELECT RAISE(ABORT,'Control Task alias fault'); END;")?;
    assert!(f
        .db
        .commit_control_with_domain_requests(&command, &work_request(), &task_request(), &ledger)
        .is_err());
    f.db.transaction(|tx| {
        assert_eq!(f.db_state(tx)?, command.expected.state);
        assert_eq!(
            tx.query_row(
                "SELECT status FROM tasks WHERE task_id='rollback-task'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "running"
        );
        for table in [
            "messages",
            "queue_entries",
            "task_result_settlements",
            "control_command_receipts",
            "control_cancellation_obligations",
            "input_query_birth_members",
            "input_query_birth_cursors",
        ] {
            assert_eq!(
                tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, usize>(0))?,
                0,
                "{table}"
            );
        }
        Ok(())
    })?;
    f.db.connection()?
        .execute_batch("DROP TRIGGER fail_control_task_alias")?;
    let (_, delta) = applied(&f, &command, &ledger)?;
    assert_eq!(
        delta
            .transition
            .effects
            .task_delta
            .as_ref()
            .unwrap()
            .revision,
        old.coverage().revision + 1
    );
    drop((delta, old));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_task_pool_budget_precedes_changed_snapshot_decode_and_business_sql() -> Result<()> {
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let old = seed_task(&f, &running(&f, "oversized-task"), true, &ledger)?;
    let command = f.command(ControlAction::Stop);
    f.db.connection()?.execute("UPDATE tasks SET payload_json='{'||hex(zeroblob(1024*1024)) WHERE task_id='oversized-task'", [])?;
    let mut limited = task_request();
    limited.byte_limit = 1024 * 1024;
    let error =
        f.db.commit_control_with_domain_requests(&command, &work_request(), &limited, &ledger)
            .err()
            .context("large malformed current Task unexpectedly accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM control_command_receipts", [], |r| {
                r.get::<_, usize>(0)
            })?,
        0
    );
    assert_eq!(
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    drop(old);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_interrupt_reobserves_activation_metadata_without_task_or_result_writes() -> Result<()> {
    use crate::runtime_db::{
        input_evidence_snapshot::{register_committed_input_evidence_tx, CanonicalInputEvidence},
        task_result_settlement::{
            self as result, TaskResultSettlementRecord, TaskResultWriteLimits,
        },
    };
    let f = Fixture::new();
    f.seed_command_claim(true, false);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let terminal = f.task_result(&running(&f, "already-terminal-task"));
    let (before, result_id) = f.db.transaction(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &scope(&f),
            &task_request(),
            &ledger,
        )?
        .require_current()?;
        let record = TaskResultSettlementRecord::pending(
            &terminal.task,
            &terminal.message,
            terminal.message.created_at,
        )?
        .context("original Result missing")?;
        let keys = BTreeSet::from([
            TaskKey::new(TaskFamily::Task, &terminal.task.id),
            TaskKey::new(TaskFamily::ResultSettlement, &record.result_identity),
        ]);
        let proof = tasks::prepare_task_write_tx(tx, &scope(&f), &keys)?;
        upsert_task_tx(tx, &terminal.task)?;
        let (source, inserted) = append_message_tx(tx, &terminal.message)?;
        assert!(inserted);
        register_committed_input_evidence_tx(
            tx,
            &scope(&f),
            &[CanonicalInputEvidence::Message(source.clone())],
        )?;
        result::prepare_upsert_pending_tx(
            tx,
            &terminal.task,
            &record,
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        let admitted = result::prepare_admit_message_tx(
            tx,
            &f.key.agent_id,
            &source.id,
            "attempt-1",
            terminal.message.created_at,
            TaskResultWriteLimits::default(),
        )?
        .execute()?;
        assert!(matches!(
            admitted.outcome,
            result::TaskResultWriteOutcome::Record(Some(_))
        ));
        tasks::register_task_writes_with_budget_tx(
            tx,
            &proof,
            &keys,
            &f.db_state(tx)?,
            &task_request(),
            &ledger,
        )?;
        let current = tasks::prepare_task_domain_snapshot_with_budget_tx(
            tx,
            &scope(&f),
            &task_request(),
            &ledger,
        )?
        .require_current()?
        .resident;
        Ok((current, record.result_identity))
    })?;
    assert!(before.coverage().result_metadata[&result_id].activation_open);
    let command = f.command(ControlAction::Stop);
    assert!(command.task_results.is_empty());
    let (_, delta) = applied(&f, &command, &ledger)?;
    assert!(delta.transition.effects.task_write_keys.is_empty());
    assert_eq!(
        delta.transition.effects.task_observation_keys,
        BTreeSet::from([TaskKey::new(TaskFamily::ResultSettlement, &result_id)])
    );
    let facts = delta
        .transition
        .effects
        .task_delta
        .as_ref()
        .context("activation change omitted Task delta")?;
    assert_eq!(facts.revision, before.coverage().revision + 1);
    assert!(!facts.coverage.result_metadata[&result_id].activation_open);
    let after = before.with_committed(facts)?;
    assert!(!after.coverage().result_metadata[&result_id].activation_open);
    assert!(before.coverage().result_metadata[&result_id].activation_open);
    drop((before, after, delta));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_ack_keeps_fresh_stop_arcs_observed_until_root_or_drop() -> Result<()> {
    use crate::runtime_db::{
        input_mutation_receipts::{
            canonical_command_digest, insert_input_mutation_receipt_tx, InputMutationFamily,
            InputMutationOutcome, InputMutationResult, InputMutationScopeOutcome,
            PreparedInputMutation,
        },
        task_stop::{
            insert_task_stop_with_observation_ack_tx, prepare_task_stop_tx, PreparedTaskStop,
            TaskStopSignal,
        },
    };
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let old = seed_task(&f, &running(&f, "pending-stop-control"), true, &ledger)?;
    drop(old);
    f.db.transaction(|tx| {
        let scope = scope(&f);
        let request = task_request();
        let key = TaskKey::new(TaskFamily::Task, "pending-stop-control");
        let resident =
            tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &scope, &request, &ledger)?
                .require_current()?
                .resident;
        let captured = resident.capture(&TaskCaptureRequest::current())?;
        let task = captured.lookup(&key)?.context("original Task missing")?;
        let CanonicalTaskFact::Task(before) = &task.fact else {
            anyhow::bail!("wrong family")
        };
        let intent = PreparedTaskStop {
            task: task.locator.clone(),
            kind: before.kind,
            created_at: before.created_at,
            rejoin: before.rejoin_fence().ok(),
            owner_epoch: 1,
            signal: TaskStopSignal::Cancel,
            lease_id: Some("original-command-lease".into()),
            child: None,
            child_unavailable: None,
            worktree: None,
            worktree_unavailable: None,
            result: None,
        };
        let prepared = PreparedInputMutation::new(
            InputMutationFamily::Task,
            "original-task-stop-before-control".into(),
            vec![scope.clone()],
            canonical_command_digest(&intent)?,
            f.time,
        )?;
        let keys = BTreeSet::from([key]);
        let proof = tasks::prepare_task_write_tx(tx, &scope, &keys)?;
        let stop_proof = prepare_task_stop_tx(tx, &prepared, &intent, &proof, &request, &ledger)?;
        let mut cancelling = before.clone();
        cancelling.status = TaskStatus::Cancelling;
        cancelling.detail.as_mut().unwrap()["cancel_requested"] = json!(true);
        upsert_task_tx(tx, &cancelling)?;
        let (record, inserted) =
            insert_task_stop_with_observation_ack_tx(&stop_proof, &cancelling)?;
        tasks::register_task_writes_with_stop_observation_tx(
            tx,
            &proof,
            &keys,
            &f.db_state(tx)?,
            &request,
            &ledger,
            None,
            &inserted,
        )?;
        insert_input_mutation_receipt_tx(
            tx,
            &prepared,
            &InputMutationOutcome {
                result: InputMutationResult::Task {
                    task_id: cancelling.id,
                    deletion: None,
                    stop: Some(record.receipt_binding()),
                },
                applied: true,
                delivery_receipt: None,
                scopes: vec![InputMutationScopeOutcome {
                    scope,
                    install_keys: Default::default(),
                    audit_events: vec![],
                    notify_memory_index: false,
                    notify_scheduler: false,
                }],
            },
            f.time,
        )?;
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (_, delta) = applied(&f, &f.command(ControlAction::Stop), &ledger)?;
    let task_delta = delta
        .transition
        .effects
        .task_delta
        .as_ref()
        .context("Control Task delta missing")?;
    assert_eq!(task_delta.stop_records.len(), 1);
    let weak = Arc::downgrade(&task_delta.stop_records[0]);
    assert!(
        ledger.report()?.physical_records.bytes >= task_delta.stop_records[0].resident_bytes()?
    );
    assert_eq!(
        ledger.report()?.retained_unclassified.allocations,
        0,
        "fresh Stop ARC must retain its Candidate view through the Control ACK"
    );
    let views = delta.transition.effects.task_material_budget_views.clone();
    drop(delta);
    assert!(
        weak.upgrade().is_none(),
        "budget guard must not retain sidecar body"
    );
    drop(views);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_task_result_source_failure_rolls_back_original_ack_and_single_finalizer() -> Result<()> {
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let old = seed_task(&f, &running(&f, "source-fault-task"), true, &ledger)?;
    let command = f.command(ControlAction::Stop);
    f.db.connection()?.execute_batch(
        "CREATE TRIGGER fail_control_result_source BEFORE INSERT ON task_message_order_headers          WHEN NEW.evidence_id='result:source-fault-task'          BEGIN SELECT RAISE(ABORT,'original Control Result source fault'); END;",
    )?;
    let error = f
        .db
        .commit_control_with_domain_requests(&command, &work_request(), &task_request(), &ledger)
        .unwrap_err();
    assert!(format!("{error:#}").contains("original Control Result source fault"));
    f.db.transaction(|tx| {
        assert_eq!(f.db_state(tx)?, command.expected.state);
        assert_eq!(
            tx.query_row(
                "SELECT status FROM tasks WHERE task_id='source-fault-task'",
                [],
                |row| row.get::<_, String>(0),
            )?,
            "running"
        );
        for table in [
            "messages",
            "queue_entries",
            "task_result_settlements",
            "control_command_receipts",
            "control_cancellation_obligations",
            "task_message_order_headers",
            "task_message_sources",
        ] {
            assert_eq!(
                tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, usize>(0))?,
                0,
                "{table}"
            );
        }
        let fresh = tasks::load_task_domain_snapshot_tx(tx, &scope(&f), &task_request())?
            .require_current()?
            .resident;
        assert_eq!(fresh.coverage(), old.coverage());
        Ok(())
    })?;
    drop(old);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_explicit_partial_generations_validate_the_original_terminal_once() -> Result<()> {
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let mut original = running(&f, "partial-control-task");
    original
        .detail
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("parent_turn_id");
    let old = seed_task(&f, &original, true, &ledger)?;
    let mut input = f.input(ControlAction::Stop);
    let planned = &input.task_results[0].task;
    let before = crate::runtime::task_generation::TaskRejoinGeneration::of(&original).unwrap();
    let terminal = crate::runtime::task_generation::TaskRejoinGeneration::of(planned).unwrap();
    let target = &mut input.cancellation_targets[0].target_generation;
    target[crate::runtime::task_generation::CONTROL_ORIGINAL_REJOIN] =
        serde_json::to_value(before)?;
    target[crate::runtime::task_generation::CONTROL_TERMINAL_REJOIN] =
        serde_json::to_value(terminal)?;
    let command = prepare_control(input)?;
    let (receipt, delta) = applied(&f, &command, &ledger)?;
    assert_eq!(
        delta.tasks[0].detail.as_ref().unwrap()["rejoin_generation"],
        json!(1)
    );
    assert!(original
        .detail
        .as_ref()
        .unwrap()
        .get("parent_turn_id")
        .is_none());
    assert_eq!(
        delta.tasks[0].detail.as_ref().unwrap()["parent_turn_id"],
        json!(delta.result_messages[0].turn_id.as_ref().unwrap())
    );
    let target = &command.cancellation_targets[0];
    assert!(
        crate::runtime::task_generation::control_original_rejoin_matches(
            &original,
            &target.target_generation
        )
    );
    assert!(
        crate::runtime::task_generation::control_terminal_rejoin_matches(
            &delta.tasks[0],
            &target.target_generation
        )
    );
    let facts = delta
        .transition
        .effects
        .task_delta
        .as_ref()
        .context("Task delta missing")?;
    assert_eq!(facts.revision, old.coverage().revision + 1);
    f.db.transaction(|tx| {
        assert_eq!(
            tasks::load_task_domain_snapshot_tx(tx, &scope(&f), &task_request())?
                .require_current()?
                .resident
                .coverage(),
            &facts.coverage
        );
        Ok(())
    })?;
    assert!(
        matches!(f.db.commit_control_with_domain_requests(&command,&work_request(),&task_request(),&ledger)?,
        ControlCommit::Replayed {receipt: original} if original == receipt)
    );
    drop((delta, old));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn legacy_control_whole_receipt_replay_never_redecorates_or_reads_a_current_task_body() -> Result<()>
{
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let mut original = running(&f, "legacy-partial-control");
    original
        .detail
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("parent_turn_id");
    let old = seed_task(&f, &original, true, &ledger)?;
    // Original stored contract intentionally has no new raw-generation fields.
    let command = f.command(ControlAction::Stop);
    assert!(command.cancellation_targets[0]
        .target_generation
        .get(crate::runtime::task_generation::CONTROL_ORIGINAL_REJOIN)
        .is_none());
    let (receipt, delta) = applied(&f, &command, &ledger)?;
    let (original_receipt,original_target):(String,String)=f.db.connection()?.query_row(
        "SELECT r.receipt_json,o.target_json FROM control_command_receipts r JOIN control_cancellation_obligations o USING(agent_id,identity_incarnation,command_id) WHERE o.target_kind='task'",
        [],|row|Ok((row.get(0)?,row.get(1)?)))?;
    // Simulate unavailable current storage: exact old outcome must exit first.
    f.db.connection()?
        .execute_batch("ALTER TABLE tasks RENAME TO legacy_current_task_body_unavailable")?;
    let replay = f.db.commit_control_with_domain_requests(
        &command,
        &work_request(),
        &task_request(),
        &ledger,
    );
    f.db.connection()?
        .execute_batch("ALTER TABLE legacy_current_task_body_unavailable RENAME TO tasks")?;
    assert!(matches!(replay?,ControlCommit::Replayed {receipt: original} if original == receipt));
    let after:(String,String)=f.db.connection()?.query_row(
        "SELECT r.receipt_json,o.target_json FROM control_command_receipts r JOIN control_cancellation_obligations o USING(agent_id,identity_incarnation,command_id) WHERE o.target_kind='task'",
        [],|row|Ok((row.get(0)?,row.get(1)?)))?;
    assert_eq!(after, (original_receipt, original_target));
    assert!(
        !crate::runtime::task_generation::control_original_rejoin_matches(
            &original,
            &command.cancellation_targets[0].target_generation
        ),
        "old null outcome cannot be rebound to a current partial producer"
    );
    drop((delta, old));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
