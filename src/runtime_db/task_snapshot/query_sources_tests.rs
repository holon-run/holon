use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::input_evidence_budget::{
        InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
    },
    runtime_db::{
        evidence::append_message_tx,
        input_evidence_snapshot::{register_committed_input_evidence_tx, CanonicalInputEvidence},
    },
    types::{AuthorityClass, MessageBody, MessageEnvelope, Priority},
};

// Reuse the original Task qualification fixture; source rows are written by
// actual append+D3 ACK, not invented index entries or body-read permission.
use crate::runtime_db::task_snapshot::tests::{at, fresh, request, scope, task};

fn task_locator(tx: &Transaction<'_>) -> Result<TaskFactLocator> {
    let task = task("output-task", crate::types::TaskStatus::Running);
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    repositories::upsert_task_tx(tx, &task)?;
    let ack = register_task_writes_tx(
        tx,
        &proof,
        &keys,
        &crate::types::AgentState::new(&scope().agent_id),
        &request(),
    )?;
    Ok(ack
        .delta
        .records
        .iter()
        .find(|r| r.locator.key.family == TaskFamily::Task)
        .context("Task ACK missing")?
        .locator
        .clone())
}
fn message(
    tx: &Transaction<'_>,
    id: &str,
    kind: MessageKind,
    origin: MessageOrigin,
    task_id: Option<&str>,
    seq: Option<u64>,
) -> Result<()> {
    let mut message = MessageEnvelope::new(
        &scope().agent_id,
        kind,
        origin,
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text { text: id.into() },
    );
    message.id = id.into();
    message.created_at = at();
    message.metadata = task_id.map(|id| serde_json::json!({"task_id":id}));
    let (message, _) = append_message_tx(tx, &message)?;
    // A deliberate equal-order fixture retains typed canonical consistency.
    // D3 actual readback is acknowledged after the normalized sequence update.
    let message = if let Some(seq) = seq {
        let mut changed = message;
        changed.message_seq = (seq > 0).then_some(seq);
        let raw = serde_json::to_string(&changed)?;
        tx.execute("UPDATE messages SET message_seq=?1,payload_json=?2,content_hash=?3 WHERE evidence_id=?4",params![changed.message_seq,raw,crate::runtime_db::evidence::content_hash(&raw),id])?;
        changed
    } else {
        message
    };
    register_committed_input_evidence_tx(
        tx,
        &scope(),
        &[CanonicalInputEvidence::Message(message)],
    )?;
    crate::runtime_db::input_evidence_writes::synchronize_message_count_tx(tx, &scope().agent_id)?;
    Ok(())
}
fn observe(tx: &Transaction<'_>, task: &TaskFactLocator) -> Result<TaskOutputCoverage> {
    observe_output_heads_tx(tx, &scope(), &[task], |_, _| Ok(()))
}

#[test]
fn output_source_window_result_priority_history_fallback_and_unrelated_eviction_match_original(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        let task = task_locator(tx)?;
        message(
            tx,
            "old-result",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "origin-may-differ".into(),
            },
            Some("output-task"),
            None,
        )?;
        message(
            tx,
            "recent-other",
            MessageKind::InternalFollowup,
            MessageOrigin::Task {
                task_id: "origin-may-differ".into(),
            },
            Some("output-task"),
            None,
        )?;
        assert_eq!(
            observe(tx, &task)?.head(&task)?.unwrap().message.key.id,
            "old-result"
        );
        // 199 unrelated rows evict the old Result while retaining other.
        for index in 0..199 {
            message(
                tx,
                &format!("unrelated-{index}"),
                MessageKind::SystemTick,
                MessageOrigin::System {
                    subsystem: "test".into(),
                },
                None,
                None,
            )?;
        }
        let selected = observe(tx, &task)?;
        let head = selected.head(&task)?.unwrap();
        assert_eq!(head.place, TaskOutputSourcePlace::RecentOther);
        assert_eq!(head.message.key.id, "recent-other");
        message(
            tx,
            "one-more-unrelated",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "test".into(),
            },
            None,
            None,
        )?;
        let selected = observe(tx, &task)?;
        let head = selected.head(&task)?.unwrap();
        assert_eq!(head.place, TaskOutputSourcePlace::HistoricalResult);
        assert_eq!(head.message.key.id, "old-result");
        Ok(())
    })
}

#[test]
fn output_source_recent_and_historical_ties_keep_distinct_original_id_order() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        let task = task_locator(tx)?;
        for id in ["a-result", "z-result"] {
            message(
                tx,
                id,
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: "different-origin-id".into(),
                },
                Some("output-task"),
                Some(0),
            )?;
        }
        assert_eq!(
            observe(tx, &task)?.head(&task)?.unwrap().message.key.id,
            "a-result"
        );
        for index in 0..200 {
            message(
                tx,
                &format!("window-{index}"),
                MessageKind::SystemTick,
                MessageOrigin::System {
                    subsystem: "test".into(),
                },
                None,
                None,
            )?;
        }
        let selected = observe(tx, &task)?;
        assert_eq!(selected.head(&task)?.unwrap().message.key.id, "z-result");
        Ok(())
    })
}

#[test]
fn output_source_explicit_none_requires_complete_original_inventory_and_exact_task_version(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        let task = task_locator(tx)?;
        let selected = observe(tx, &task)?;
        assert!(selected.head(&task)?.is_none());
        let mut newer = task.clone();
        newer.revision += 1;
        assert!(selected.head(&newer).is_err());
        tx.execute("DELETE FROM task_message_source_inventory", [])?;
        assert!(observe(tx, &task).is_err());
        Ok(())
    })
}

#[test]
fn output_source_scalar_budget_precedes_corrupt_locator_decode_and_releases_guard() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let task = task_locator(tx)?;
        message(
            tx,
            "head",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "output-task".into(),
            },
            Some("output-task"),
            None,
        )?;
        tx.execute(
            "UPDATE task_message_sources SET source_locator_json=?1",
            ["{".repeat(65536)],
        )?;
        let mut guards = Vec::new();
        let error = observe_output_heads_tx(tx, &scope(), &[&task], |bytes, _| {
            ensure!(
                bytes < 4096,
                TaskSnapshotError::ResourceExhausted("Task output source material")
            );
            guards.push(ledger.register_owned_bytes(
                &scope(),
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: u64::try_from(bytes)?,
                },
            )?);
            Ok(())
        })
        .unwrap_err();
        assert!(error.downcast_ref::<TaskSnapshotError>().is_some());
        drop(guards);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

fn actual_message_delta(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta> {
    let actual = crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope().agent_id)
        .message_by_id(id)?
        .context("actual source Message missing")?;
    register_committed_input_evidence_tx(tx, &scope(), &[CanonicalInputEvidence::Message(actual)])
}

#[test]
fn prepared_output_heads_metadata_only_source_ack_preserves_task_arc_and_exact_capture(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let initial = db
        .transaction(|tx| {
            task_locator(tx)?;
            super::super::loader::prepare_task_output_snapshot_with_budget_tx(
                tx,
                &scope(),
                &request(),
                &ledger,
            )?
            .require_current()
        })?
        .resident;
    let selected = initial.capture(&request().capture)?;
    assert!(selected.output_source("output-task")?.is_none());
    let original = selected
        .lookup(&TaskKey::new(TaskFamily::Task, "output-task"))?
        .unwrap()
        .clone();
    drop(selected);
    let (delta, guards) = db.transaction(|tx| {
        message(
            tx,
            "actual-result-head",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "original-origin".into(),
            },
            Some("output-task"),
            None,
        )?;
        let input = actual_message_delta(tx, "actual-result-head")?;
        super::super::register_task_source_metadata_tx(
            tx,
            &scope(),
            &input,
            &request(),
            &ledger,
            None,
        )?
        .context("metadata advance missing")
    })?;
    assert!(delta.records.is_empty() && delta.stop_records.is_empty());
    assert_eq!(delta.revision, delta.base_revision + 1);
    let next = initial.with_committed(&delta)?;
    let capture = next.capture(&request().capture)?;
    assert_eq!(
        capture
            .output_source("output-task")?
            .unwrap()
            .message
            .key
            .id,
        "actual-result-head"
    );
    assert!(Arc::ptr_eq(
        &original,
        capture
            .lookup(&TaskKey::new(TaskFamily::Task, "output-task"))?
            .unwrap()
    ));
    assert_eq!(
        capture.records.len(),
        1,
        "source Message body is not held by Task capture"
    );
    let empty = next.capture(&TaskCaptureRequest {
        include_active: false,
        include_unsettled: false,
        terminal_limit: 0,
        recovery_source_ids: BTreeSet::new(),
        required_keys: BTreeSet::new(),
    })?;
    assert!(empty.records.is_empty() && empty.coverage.output.heads.is_empty());
    assert!(
        empty.output_source("output-task").is_err(),
        "unselected head cannot claim a body"
    );
    let weak = Arc::downgrade(&original);
    drop((original, initial, next, capture, delta, guards));
    assert!(
        weak.upgrade().is_none(),
        "metadata-only empty capture has no Task/root backref"
    );
    drop(empty);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_writer_refreshes_observed_output_head_at_one_revision_without_reloading_message(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        task_locator(tx)?;
        message(
            tx,
            "writer-source",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "original".into(),
            },
            Some("output-task"),
            None,
        )?;
        let before = super::super::loader::prepare_task_output_snapshot_with_budget_tx(
            tx,
            &scope(),
            &request(),
            &ledger,
        )?
        .require_current()?
        .resident;
        let mut completed = task("output-task", crate::types::TaskStatus::Completed);
        completed.updated_at += chrono::Duration::seconds(1);
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &completed.id)]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &completed)?;
        let final_state =
            crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope().agent_id)
                .scope_facts()?
                .state
                .unwrap();
        let ack = register_task_writes_with_budget_tx(
            tx,
            &proof,
            &keys,
            &final_state,
            &request(),
            &ledger,
        )?;
        assert_eq!(ack.delta.revision, before.coverage().revision + 1);
        let next = before.with_committed(&ack.delta)?;
        let capture = next.capture(&request().capture)?;
        let record = capture
            .lookup(&TaskKey::new(TaskFamily::Task, "output-task"))?
            .unwrap();
        assert_eq!(
            capture.coverage.output.heads["output-task"].task,
            record.locator
        );
        assert_eq!(
            capture
                .output_source("output-task")?
                .unwrap()
                .message
                .key
                .id,
            "writer-source"
        );
        Ok(())
    })
}

#[test]
fn ordinary_task_preparation_maintains_output_metadata_without_adopting_raw_tail() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        task_locator(tx)?;
        let raw = MessageEnvelope::new(
            &scope().agent_id,
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "unreceived".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text { text: "raw".into() },
        );
        append_message_tx(tx, &raw)?;
        let ordinary =
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
                .require_current()?;
        let ordinary_capture = ordinary.resident.capture(&request().capture)?;
        assert!(ordinary_capture.output_source("output-task")?.is_none());
        assert!(ordinary_capture
            .coverage
            .output
            .window
            .as_ref()
            .unwrap()
            .message_ids
            .is_empty());
        let output = super::super::loader::prepare_task_output_snapshot_with_budget_tx(
            tx,
            &scope(),
            &request(),
            &ledger,
        )?
        .require_current()?
        .resident;
        let selected = output.capture(&request().capture)?;
        assert!(selected.output_source("output-task")?.is_none());
        assert!(
            selected
                .coverage
                .output
                .window
                .unwrap()
                .message_ids
                .is_empty(),
            "raw unreceived canonical history cannot enter the accepted window"
        );
        let unreceived: u64 = tx.query_row(
            "SELECT COUNT(*) FROM task_message_order_headers",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(unreceived, 0, "named output never backfills a raw tail");
        Ok(())
    })
}

#[test]
fn fixed_output_metadata_uses_original_inventory_without_alias_or_late_source_adoption(
) -> Result<()> {
    let (_dir, db) = crate::runtime_db::task_snapshot::tests::legacy(true, |tx| {
        repositories::upsert_task_tx(
            tx,
            &task("output-task", crate::types::TaskStatus::Completed),
        )?;
        let mut source = MessageEnvelope::new(
            &scope().agent_id,
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "original".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "fixed source".into(),
            },
        );
        source.id = "fixed-output-source".into();
        source.created_at = at();
        source.metadata = Some(serde_json::json!({"task_id":"output-task"}));
        append_message_tx(tx, &source)?;
        let mut state = crate::types::AgentState::new(&scope().agent_id);
        state.total_message_count = 1;
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &state)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let selected=super::super::loader::prepare_task_output_snapshot_with_budget_tx(tx,&scope(),&request(),&ledger)?.require_current()?.resident.capture(&request().capture)?;
        assert_eq!(selected.output_source("output-task")?.unwrap().message.key.id,"fixed-output-source");
        let aliases:u64=tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE family='message' AND record_id='fixed-output-source'",[],|row|row.get(0))?;
        assert_eq!(aliases,0,"selector metadata cannot grant source body authority");
        let raw=MessageEnvelope::new(&scope().agent_id,MessageKind::SystemTick,MessageOrigin::System{subsystem:"late raw".into()},AuthorityClass::RuntimeInstruction,Priority::Normal,MessageBody::Text{text:"late".into()});
        append_message_tx(tx,&raw)?;
        let unchanged = super::super::loader::prepare_task_output_snapshot_with_budget_tx(tx,&scope(),&request(),&ledger)?.require_current()?.resident.capture(&request().capture)?;
        assert_eq!(unchanged.coverage.output, selected.coverage.output, "raw late tail cannot change the accepted window or head");
        let aliases_after: u64 = tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND family='message'",params![scope().agent_id,scope().identity_incarnation],|row|row.get(0))?;
        assert_eq!(aliases_after, aliases, "metadata selection cannot adopt raw body");
        let raw_header: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM task_message_order_headers WHERE agent_id=?1 AND identity_incarnation=?2 AND evidence_id=?3)",params![scope().agent_id,scope().identity_incarnation,raw.id],|row|row.get(0))?;
        assert!(!raw_header, "raw late row cannot become an accepted header");
        Ok(())
    })
}

#[test]
fn source_metadata_budget_precedes_corrupt_coverage_decode_and_original_transaction_rolls_back(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (before, baseline) = db.transaction(|tx| {
        task_locator(tx)?;
        let before = super::super::loader::prepare_task_output_snapshot_with_budget_tx(
            tx,
            &scope(),
            &request(),
            &ledger,
        )?
        .require_current()?
        .resident
        .coverage()
        .clone();
        Ok((before, ledger.report()?.observed_bytes))
    })?;
    let error=db.transaction(|tx| {
        message(tx,"rolled-back-source",MessageKind::TaskResult,MessageOrigin::Task{task_id:"original".into()},Some("output-task"),None)?;
        let input=actual_message_delta(tx,"rolled-back-source")?;
        tx.execute("UPDATE task_domain_current SET coverage_json=?1 WHERE agent_id=?2 AND identity_incarnation=?3",params!["x".repeat(16384),scope().agent_id,scope().identity_incarnation])?;
        let mut limited=request();limited.byte_limit=1024;
        super::super::register_task_source_metadata_tx(tx,&scope(),&input,&limited,&ledger,None)?;
        Ok(())
    }).err().context("corrupt oversized metadata accepted")?;
    assert!(matches!(
        error.downcast_ref::<TaskSnapshotError>(),
        Some(TaskSnapshotError::ResourceExhausted(_))
    ));
    db.transaction(|tx| {
        assert_eq!(
            super::super::loader::current_coverage(tx, &scope())?.unwrap(),
            before
        );
        let body: u64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE evidence_id='rolled-back-source'",
            [],
            |row| row.get(0),
        )?;
        let index: u64 = tx.query_row(
            "SELECT COUNT(*) FROM task_message_sources WHERE source_id='rolled-back-source'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!((body, index), (0, 0));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

fn source_ack_work_request() -> crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
    crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: request().record_limit,
        byte_limit: request().byte_limit,
    }
}

fn prepared_source(
    id: &str,
    nonce: &str,
) -> Result<crate::runtime_db::prepared_input_evidence::PreparedInputEvidence> {
    let mut source = MessageEnvelope::new(
        &scope().agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: "output-task".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original actual source".into(),
        },
    );
    source.id = id.into();
    source.created_at = at();
    source.metadata = Some(serde_json::json!({"task_id":"output-task"}));
    crate::runtime_db::prepared_input_evidence::PreparedInputEvidence::message(
        scope(),
        nonce.into(),
        at(),
        source,
        Vec::new(),
    )
}

#[test]
fn standalone_source_ack_carries_one_metadata_task_delta_and_original_receipt_replay() -> Result<()>
{
    use crate::runtime_db::transitions::InputMutationExecution;
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let before = db.transaction(|tx| {
        task_locator(tx)?;
        Ok(
            super::super::loader::prepare_task_output_snapshot_with_budget_tx(
                tx,
                &scope(),
                &request(),
                &ledger,
            )?
            .require_current()?
            .resident,
        )
    })?;
    let original_capture = before.capture(&request().capture)?;
    let original = original_capture
        .lookup(&TaskKey::new(TaskFamily::Task, "output-task"))?
        .unwrap()
        .clone();
    let prepared = prepared_source("standalone-source", "original-source-command")?;
    let InputMutationExecution::Committed {
        value: ack,
        receipt,
    } = db
        .evidence()
        .commit_prepared_input_evidence_with_task_budget(
            &prepared,
            &source_ack_work_request(),
            &request(),
            &ledger,
            None,
            None,
        )?
    else {
        panic!("first actual Source ACK must commit")
    };
    let delta = ack
        .task_delta
        .as_ref()
        .context("Source ACK omitted observed Task heads")?;
    assert!(delta.records.is_empty() && delta.stop_records.is_empty());
    assert!(!ack.task_material_budget_views.is_empty());
    assert_eq!(delta.revision, before.coverage().revision + 1);
    let after = before.with_committed(delta)?;
    let captured = after.capture(&request().capture)?;
    assert!(Arc::ptr_eq(
        &original,
        captured
            .lookup(&TaskKey::new(TaskFamily::Task, "output-task"))?
            .unwrap()
    ));
    assert_eq!(
        captured
            .output_source("output-task")?
            .unwrap()
            .message
            .key
            .id,
        "standalone-source"
    );
    let revision = delta.revision;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = db
        .evidence()
        .commit_prepared_input_evidence_with_task_budget(
            &prepared,
            &source_ack_work_request(),
            &request(),
            &ledger,
            None,
            None,
        )?
    else {
        panic!("original whole receipt must replay")
    };
    assert_eq!(receipt, replay);
    assert_eq!(
        db.transaction(
            |tx| Ok(super::super::loader::current_coverage(tx, &scope())?
                .unwrap()
                .revision)
        )?,
        revision
    );
    drop((ack, before, after, captured, original, original_capture));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn standalone_source_ack_without_current_task_domain_does_not_bootstrap_task_delta() -> Result<()> {
    use crate::runtime_db::transitions::InputMutationExecution;
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        task_locator(tx)?;
        tx.execute(
            "DELETE FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
        )?;
        Ok(())
    })?;
    let prepared = prepared_source("unobserved-source", "unobserved-source-command")?;
    let InputMutationExecution::Committed { value: ack, .. } = db
        .evidence()
        .commit_prepared_input_evidence_with_task_budget(
            &prepared,
            &source_ack_work_request(),
            &request(),
            &ledger,
            None,
            None,
        )?
    else {
        panic!("actual Source ACK must commit")
    };
    assert!(ack.task_delta.is_none() && ack.task_material_budget_views.is_empty());
    assert!(
        db.transaction(|tx| super::super::loader::current_coverage(tx, &scope()))?
            .is_none(),
        "Source-only ACK cannot establish a missing complete Task domain"
    );
    assert!(
        ledger.report()?.observed_bytes > 0,
        "the original Current ACK still owns material"
    );
    drop(ack);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn standalone_source_ack_receipt_failure_rolls_back_message_index_and_task_heads() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let before = db.transaction(|tx| {
        task_locator(tx)?;
        super::super::loader::prepare_task_output_snapshot_with_budget_tx(
            tx,
            &scope(),
            &request(),
            &ledger,
        )?
        .require_current()?;
        Ok(super::super::loader::current_coverage(tx, &scope())?.unwrap())
    })?;
    db.connection()?.execute_batch("CREATE TRIGGER original_source_receipt_fault BEFORE INSERT ON input_mutation_receipts BEGIN SELECT RAISE(ABORT,'original source whole receipt failure'); END;")?;
    let prepared = prepared_source("rolled-back-standalone", "rolled-back-standalone-command")?;
    let error = db
        .evidence()
        .commit_prepared_input_evidence_with_task_budget(
            &prepared,
            &source_ack_work_request(),
            &request(),
            &ledger,
            None,
            None,
        )
        .err()
        .context("whole receipt fault did not abort original TX")?;
    assert!(error
        .to_string()
        .contains("original source whole receipt failure"));
    db.transaction(|tx| {
        assert_eq!(super::super::loader::current_coverage(tx, &scope())?.unwrap(), before);
        let counts:(u64,u64,u64)=tx.query_row("SELECT (SELECT COUNT(*) FROM messages WHERE evidence_id='rolled-back-standalone'),(SELECT COUNT(*) FROM task_message_sources WHERE source_id='rolled-back-standalone'),(SELECT COUNT(*) FROM input_mutation_receipts)",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        assert_eq!(counts,(0,0,0));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn source_metadata_record_budget_counts_pending_stop_and_reverse_collections_before_decode(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let before = db.transaction(|tx| {
        task_locator(tx)?;
        super::super::loader::prepare_task_output_snapshot_with_budget_tx(
            tx,
            &scope(),
            &request(),
            &ledger,
        )?
        .require_current()?;
        Ok(super::super::loader::current_coverage(tx, &scope())?.unwrap())
    })?;
    for field in [
        "pending_effect_seqs",
        "records",
        "reverse",
        "recovery_sources",
        "paused_replays",
    ] {
        let mut bounded = request();
        bounded.record_limit = 32;
        let error=db.transaction(|tx| {
            message(tx,"metadata-budget-source",MessageKind::TaskResult,MessageOrigin::Task{task_id:"output-task".into()},Some("output-task"),None)?;
            let mut corrupt=serde_json::to_value(&before)?;
            match field {
                "pending_effect_seqs" => corrupt["stop"]["pending_effect_seqs"] = serde_json::json!((1..=64).collect::<Vec<u64>>()),
                "records" => corrupt["stop"]["records"] = serde_json::Value::Object((1..=64).map(|n|(n.to_string(),serde_json::json!({"malformed":"must not decode"}))).collect()),
                "reverse" => corrupt["task_result_ids"]["output-task"] = serde_json::json!((0..64).map(|n|format!("result-{n}")).collect::<Vec<_>>()),
                "recovery_sources" => corrupt["recovery_sources"] = serde_json::Value::Object((0..64).map(|n|(n.to_string(),serde_json::json!({"malformed":"must not decode"}))).collect()),
                "paused_replays" => corrupt["recovery_sources"]["source"] = serde_json::json!({
                    "paused_replays": (0..64).map(|n|(n.to_string(),serde_json::json!({"malformed":"must not decode"}))).collect::<serde_json::Map<_,_>>()
                }),
                _ => unreachable!(),
            }
            tx.execute("UPDATE task_domain_current SET coverage_json=?1 WHERE agent_id=?2 AND identity_incarnation=?3",params![serde_json::to_string(&corrupt)?,scope().agent_id,scope().identity_incarnation])?;
            let input=actual_message_delta(tx,"metadata-budget-source")?;
            super::super::register_task_source_metadata_tx(tx,&scope(),&input,&bounded,&ledger,None)?;
            Ok(())
        }).err().context("full owned metadata count was not bounded")?;
        assert!(
            matches!(
                error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::ResourceExhausted(_))
            ),
            "{field}: {error:#}"
        );
        db.transaction(|tx| {
            assert_eq!(
                super::super::loader::current_coverage(tx, &scope())?.unwrap(),
                before
            );
            let count: u64 = tx.query_row(
                "SELECT COUNT(*) FROM messages WHERE evidence_id='metadata-budget-source'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0);
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn fixed_output_order_keeps_received_headers_after_unrelated_raw_member_drift() -> Result<()> {
    let (_dir, db) = crate::runtime_db::task_snapshot::tests::legacy(true, |tx| {
        repositories::upsert_task_tx(
            tx,
            &task("output-task", crate::types::TaskStatus::Completed),
        )?;
        for index in 1..=201 {
            let kind = if index == 1 {
                MessageKind::TaskResult
            } else if index == 2 {
                MessageKind::InternalFollowup
            } else {
                MessageKind::SystemTick
            };
            let origin = if index <= 2 {
                MessageOrigin::Task {
                    task_id: "output-task".into(),
                }
            } else {
                MessageOrigin::System {
                    subsystem: "unrelated-original".into(),
                }
            };
            let mut source = MessageEnvelope::new(
                &scope().agent_id,
                kind,
                origin,
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("original-{index}"),
                },
            );
            source.id = format!("fixed-order-{index:03}");
            source.created_at = at();
            source.metadata = (index <= 2).then(|| serde_json::json!({"task_id":"output-task"}));
            append_message_tx(tx, &source)?;
        }
        let mut state = crate::types::AgentState::new(&scope().agent_id);
        state.total_message_count = 201;
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &state)
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let original = db.transaction(|tx| {
        Ok(
            super::super::loader::prepare_task_output_snapshot_with_budget_tx(
                tx,
                &scope(),
                &request(),
                &ledger,
            )?
            .require_current()?
            .resident,
        )
    })?;
    let before = original.capture(&request().capture)?;
    assert_eq!(
        before.output_source("output-task")?.unwrap().place,
        TaskOutputSourcePlace::RecentOther
    );
    assert_eq!(
        before.output_source("output-task")?.unwrap().message.key.id,
        "fixed-order-002"
    );
    // A/B and their original fingerprints remain unchanged. Unrelated U was
    // inside original top200; raw seq0 pushes it out and would promote A.
    db.connection()?.execute(
        "UPDATE messages SET message_seq=0 WHERE evidence_id='fixed-order-003'",
        [],
    )?;
    let unchanged = db.transaction(|tx| {
        Ok(
            super::super::loader::prepare_task_output_snapshot_with_budget_tx(
                tx,
                &scope(),
                &request(),
                &ledger,
            )?
            .require_current()?
            .resident,
        )
    })?;
    let unchanged = unchanged.capture(&request().capture)?;
    assert_eq!(
        unchanged.coverage.output.window,
        before.coverage.output.window
    );
    assert_eq!(
        unchanged
            .output_source("output-task")?
            .unwrap()
            .message
            .key
            .id,
        "fixed-order-002"
    );
    db.transaction(|tx| {
        assert_eq!(super::super::loader::current_coverage(tx,&scope())?.unwrap(),*original.coverage());
        let headers:(u64,u64)=tx.query_row("SELECT (SELECT COUNT(*) FROM task_message_order_headers WHERE agent_id=?1 AND identity_incarnation=?2),(SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2)",params![scope().agent_id,scope().identity_incarnation],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(headers,(201,0),"fixed order metadata never mints Input body aliases");
        Ok(())
    })?;
    db.connection()?.execute(
        "UPDATE messages SET message_seq=3 WHERE evidence_id='fixed-order-003'",
        [],
    )?;
    db.transaction(|tx| {
        let restored = super::super::loader::prepare_task_output_snapshot_with_budget_tx(
            tx,
            &scope(),
            &request(),
            &ledger,
        )?
        .require_current()?
        .resident;
        assert_eq!(
            restored
                .capture(&request().capture)?
                .output_source("output-task")?
                .unwrap()
                .message
                .key
                .id,
            "fixed-order-002"
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn actual_message_ack_registers_non_task_order_headers_and_raw_updates_do_not_rewrite_them(
) -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        let task = task_locator(tx)?;
        message(tx,"actual-other",MessageKind::SystemTick,MessageOrigin::System{subsystem:"unrelated".into()},None,None)?;
        let received:(u64,u64)=tx.query_row("SELECT (SELECT COUNT(*) FROM task_message_order_headers),(SELECT COUNT(*) FROM task_message_sources)",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(received,(1,0),"every actual Message contributes order, only Task origin contributes a Task source");
        let header:Option<u64>=tx.query_row("SELECT message_seq FROM task_message_order_headers WHERE evidence_id='actual-other'",[],|row|row.get(0))?;
        tx.execute("UPDATE messages SET message_seq=999 WHERE evidence_id='actual-other'",[])?;
        let unchanged:Option<u64>=tx.query_row("SELECT message_seq FROM task_message_order_headers WHERE evidence_id='actual-other'",[],|row|row.get(0))?;
        assert_eq!(header,unchanged);
        let accepted = observe(tx, &task)?;
        assert_eq!(accepted.window.unwrap().message_ids,vec!["actual-other"]);
        assert!(accepted.heads["output-task"].source.is_none());
        Ok(())
    })
}

#[test]
fn historical_source_unavailability_does_not_promote_other_and_does_not_block_task_writer(
) -> Result<()> {
    for mode in ["actual_prune", "changed_version", "missing_alias"] {
        let (_dir, db) = fresh()?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let before = db.transaction(|tx| {
            task_locator(tx)?;
            message(
                tx,
                "history-result",
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: "output-task".into(),
                },
                Some("output-task"),
                None,
            )?;
            message(
                tx,
                "history-other",
                MessageKind::InternalFollowup,
                MessageOrigin::Task {
                    task_id: "output-task".into(),
                },
                Some("output-task"),
                None,
            )?;
            for index in 0..200 {
                message(
                    tx,
                    &format!("later-unrelated-{index}"),
                    MessageKind::SystemTick,
                    MessageOrigin::System {
                        subsystem: "unrelated".into(),
                    },
                    None,
                    None,
                )?;
            }
            Ok(
                super::super::loader::prepare_task_output_snapshot_with_budget_tx(
                    tx,
                    &scope(),
                    &request(),
                    &ledger,
                )?
                .require_current()?
                .resident,
            )
        })?;
        let initial = before.capture(&request().capture)?;
        let selected = initial.output_source("output-task")?.unwrap();
        assert_eq!(
            (selected.place, selected.message.key.id.as_str()),
            (TaskOutputSourcePlace::HistoricalResult, "history-result")
        );
        let (delta,guards) = db.transaction(|tx| {
            let input = match mode {
                "actual_prune" => {
                    tx.execute("DELETE FROM messages WHERE evidence_id='history-result'",[])?;
                    let removed = crate::runtime_db::input_evidence_snapshot::register_pruned_input_evidence_tx(tx,&scope(),&[InputEvidenceKey::new(InputEvidenceFamily::Message,"history-result")])?;
                    assert_eq!(removed.removals.len(),1,"use actual original qualified D3 prune ACK");
                    crate::runtime_db::input_evidence_writes::synchronize_message_count_tx(tx,&scope().agent_id)?;
                    removed
                }
                "changed_version" | "missing_alias" => {
                    // Negative override injection: keep accepted Source metadata
                    // but corrupt/remove its current Input version, not its body.
                    if mode == "changed_version" {
                        tx.execute("UPDATE input_evidence_current SET revision=revision+1 WHERE agent_id=?1 AND family='message' AND record_id='history-result'",[&scope().agent_id])?;
                    } else {
                        tx.execute("DELETE FROM input_evidence_current WHERE agent_id=?1 AND family='message' AND record_id='history-result'",[&scope().agent_id])?;
                    }
                    message(tx,"actual-window-invalidation",MessageKind::SystemTick,MessageOrigin::System{subsystem:"actual-ack".into()},None,None)?;
                    actual_message_delta(tx,"actual-window-invalidation")?
                }
                _ => unreachable!(),
            };
            super::super::register_task_source_metadata_tx(tx,&scope(),&input,&request(),&ledger,None)?.context("availability metadata advance missing")
        })?;
        let next = before.with_committed(&delta)?;
        let captured = next.capture(&request().capture)?;
        assert!(task_record_present(&captured, "output-task")?);
        let head = captured.coverage.output.heads["output-task"]
            .source
            .as_ref()
            .unwrap();
        assert_eq!(
            (head.place, head.message.key.id.as_str()),
            (TaskOutputSourcePlace::HistoricalResult, "history-result")
        );
        let expected = if mode == "actual_prune" {
            TaskOutputSourceAvailability::Pruned
        } else {
            TaskOutputSourceAvailability::MissingVersion
        };
        assert_eq!(head.availability, expected);
        let error = captured
            .output_source("output-task")
            .err()
            .context("unavailable historical Result promoted older Other")?;
        assert!(
            matches!(error.downcast_ref::<TaskReadError>(),Some(TaskReadError::UnavailableOutputSource(value)) if *value==expected)
        );
        db.transaction(|tx| {
            let mut completed = task("output-task",crate::types::TaskStatus::Completed);
            completed.updated_at += chrono::Duration::seconds(1);
            let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task,&completed.id)]);
            let proof = prepare_task_write_tx(tx,&scope(),&keys)?;
            repositories::upsert_task_tx(tx,&completed)?;
            let final_state = crate::runtime_db::context_inputs::ContextDurableRead::new(tx,&scope().agent_id).scope_facts()?.state.unwrap();
            let ack = register_task_writes_with_budget_tx(tx,&proof,&keys,&final_state,&request(),&ledger)?;
            let final_domain = next.with_committed(&ack.delta)?;
            let final_capture = final_domain.capture(&request().capture)?;
            let record = final_capture.lookup(&TaskKey::new(TaskFamily::Task,"output-task"))?.unwrap();
            assert!(matches!(&record.fact,CanonicalTaskFact::Task(task) if task.status==crate::types::TaskStatus::Completed));
            assert_eq!(final_capture.coverage.output.heads["output-task"].source.as_ref().unwrap().availability,expected);
            assert!(final_capture.output_source("output-task").is_err());
            Ok(())
        })?;
        drop((before, initial, next, captured, delta, guards));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

fn task_record_present(capture: &CapturedTasks, id: &str) -> Result<bool> {
    Ok(capture
        .lookup(&TaskKey::new(TaskFamily::Task, id))?
        .is_some())
}

#[test]
fn fixed_source_initialization_preserves_historical_tombstone_and_overridden_version_as_unavailable(
) -> Result<()> {
    for mode in ["pruned", "different_version"] {
        let (_dir, db) = crate::runtime_db::task_snapshot::tests::legacy(true, |tx| {
            repositories::upsert_task_tx(
                tx,
                &task("output-task", crate::types::TaskStatus::Completed),
            )?;
            for index in 1..=202 {
                let mut source = MessageEnvelope::new(
                    &scope().agent_id,
                    if index == 1 {
                        MessageKind::TaskResult
                    } else if index == 2 {
                        MessageKind::InternalFollowup
                    } else {
                        MessageKind::SystemTick
                    },
                    if index <= 2 {
                        MessageOrigin::Task {
                            task_id: "output-task".into(),
                        }
                    } else {
                        MessageOrigin::System {
                            subsystem: "original".into(),
                        }
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: format!("fixed-{index}"),
                    },
                );
                source.id = format!("fixed-unavailable-{index:03}");
                source.created_at = at();
                source.metadata =
                    (index <= 2).then(|| serde_json::json!({"task_id":"output-task"}));
                append_message_tx(tx, &source)?;
            }
            let mut state = crate::types::AgentState::new(&scope().agent_id);
            state.total_message_count = 202;
            crate::runtime_db::evidence::upsert_agent_state_tx(tx, &state)
        })?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            // Inject a stronger negative/current-version fence before first
            // materialization; this cannot grant a Source body or a new alias.
            let seed:String=tx.query_row("SELECT seed_id FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2",params![scope().agent_id,scope().identity_incarnation],|row|row.get(0))?;
            let inventory=crate::runtime_db::episode_snapshot::task_checkpoint::load(tx,&seed,"upgrade")?.query_inventory.context("fixed inventory missing")?;
            tx.execute("INSERT INTO input_evidence_current(agent_id,identity_incarnation,family,record_id,revision,content_digest,fingerprint_json,availability,qualification,seed_id) SELECT ?1,?2,'message',f.evidence_id,2,?3,f.fingerprint_json,?4,'legacy_prepared',?6 FROM task_query_fixed_messages f WHERE f.inventory_id=?5 AND f.evidence_id='fixed-unavailable-001'",params![scope().agent_id,scope().identity_incarnation,"0".repeat(64),if mode=="pruned" {"pruned"} else {"present"},inventory.id,seed])?;
            let injected:u64=tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2 AND record_id='fixed-unavailable-001'",params![scope().agent_id,scope().identity_incarnation],|row|row.get(0))?;
            assert_eq!(injected,1,"negative override must exist before initialization");
            let loaded=super::super::loader::prepare_task_output_snapshot_with_budget_tx(tx,&scope(),&request(),&ledger)?.require_current()?.resident;
            let capture=loaded.capture(&request().capture)?;
            let head=capture.coverage.output.heads["output-task"].source.as_ref().unwrap();
            assert_eq!((head.place,head.message.key.id.as_str()),(TaskOutputSourcePlace::HistoricalResult,"fixed-unavailable-001"));
            assert_eq!(head.availability,if mode=="pruned" {TaskOutputSourceAvailability::Pruned} else {TaskOutputSourceAvailability::MissingVersion});
            assert!(capture.output_source("output-task").is_err(),"cannot promote historical Other or expose unavailable Result");
            let count:u64=tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE agent_id=?1 AND identity_incarnation=?2",params![scope().agent_id,scope().identity_incarnation],|row|row.get(0))?;
            assert_eq!(count,1,"metadata initialization cannot manufacture any Source body grant");
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn actual_non_task_ack_removes_fixed_source_before_or_after_initialization() -> Result<()> {
    for initialize_first in [false, true] {
        let (_dir, db) = crate::runtime_db::task_snapshot::tests::legacy(true, |tx| {
            repositories::upsert_task_tx(
                tx,
                &task("output-task", crate::types::TaskStatus::Completed),
            )?;
            for (id, kind) in [
                ("fixed-original-result", MessageKind::TaskResult),
                ("fixed-original-other", MessageKind::InternalFollowup),
            ] {
                let mut source = MessageEnvelope::new(
                    &scope().agent_id,
                    kind,
                    MessageOrigin::Task {
                        task_id: "output-task".into(),
                    },
                    AuthorityClass::RuntimeInstruction,
                    Priority::Normal,
                    MessageBody::Text { text: id.into() },
                );
                source.id = id.into();
                source.created_at = at();
                source.metadata = Some(serde_json::json!({"task_id":"output-task"}));
                append_message_tx(tx, &source)?;
            }
            let mut state = crate::types::AgentState::new(&scope().agent_id);
            state.total_message_count = 2;
            crate::runtime_db::evidence::upsert_agent_state_tx(tx, &state)
        })?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        db.transaction(|tx| {
            if initialize_first {
                let before = super::super::loader::prepare_task_output_snapshot_with_budget_tx(
                    tx, &scope(), &request(), &ledger,
                )?.require_current()?.resident.capture(&request().capture)?;
                assert_eq!(before.output_source("output-task")?.unwrap().message.key.id, "fixed-original-result");
            }
            // Use the actual canonical writer/readback and D3 ACK. A raw
            // mutation alone must never override fixed source membership.
            let mut changed = crate::runtime_db::evidence::committed_message_tx(
                tx, &scope().agent_id, "fixed-original-result",
            )?;
            changed.kind = MessageKind::SystemTick;
            changed.origin = MessageOrigin::System { subsystem: "actual source update".into() };
            changed.metadata = None;
            crate::runtime_db::evidence::upsert_message_tx(tx, &changed)?;
            let changed = crate::runtime_db::evidence::committed_message_tx(
                tx, &scope().agent_id, &changed.id,
            )?;
            register_committed_input_evidence_tx(
                tx, &scope(), &[CanonicalInputEvidence::Message(changed)],
            )?;
            for _ in 0..2 {
                let current = super::super::loader::prepare_task_output_snapshot_with_budget_tx(
                    tx, &scope(), &request(), &ledger,
                )?.require_current()?.resident.capture(&request().capture)?;
                let head = current.output_source("output-task")?.unwrap();
                assert_eq!(head.message.key.id, "fixed-original-other");
                assert_eq!(head.availability, TaskOutputSourceAvailability::Present);
                let resurrected: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM task_message_sources WHERE agent_id=?1 AND identity_incarnation=?2 AND source_id='fixed-original-result')",
                    params![scope().agent_id, scope().identity_incarnation], |row| row.get(0),
                )?;
                assert!(!resurrected, "fixed initialization must retain the actual removal");
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn current_policy_keeps_task_output_heads_through_original_task_and_source_acks_without_body_sql(
) -> Result<()> {
    use crate::runtime_db::transitions::InputMutationExecution;
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let before = db.transaction(|tx| {
        task_locator(tx)?;
        Ok(
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
                .require_current()?
                .resident,
        )
    })?;
    assert!(before
        .capture(&request().capture)?
        .output_source("output-task")?
        .is_none());
    let prepared = prepared_source("current-output-source", "current-output-source-ack")?;
    let InputMutationExecution::Committed { value: ack, .. } = db
        .evidence()
        .commit_prepared_input_evidence_with_task_budget(
            &prepared,
            &source_ack_work_request(),
            &request(),
            &ledger,
            None,
            None,
        )?
    else {
        panic!("actual source ACK must commit")
    };
    let after_source = before.with_committed(
        ack.task_delta
            .as_ref()
            .context("current Source ACK omitted Task metadata")?,
    )?;
    assert_eq!(
        after_source
            .capture(&request().capture)?
            .output_source("output-task")?
            .unwrap()
            .message
            .key
            .id,
        "current-output-source"
    );
    let task_ack = db.transaction(|tx| {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "output-task")]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        let mut changed = repositories::decode_task_payload(&tx.query_row(
            "SELECT payload_json FROM tasks WHERE task_id='output-task'",
            [],
            |row| row.get::<_, String>(0),
        )?)?;
        changed.status = crate::types::TaskStatus::Completed;
        changed.updated_at += chrono::Duration::seconds(1);
        repositories::upsert_task_tx(tx, &changed)?;
        let state =
            crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &scope().agent_id)
                .scope_facts()?
                .state
                .context("actual final State missing")?;
        register_task_writes_with_budget_tx(tx, &proof, &keys, &state, &request(), &ledger)
    })?;
    let current = after_source.with_committed(&task_ack.delta)?;
    assert_eq!(
        task_ack.delta.revision,
        after_source.coverage().revision + 1
    );
    db.transaction(|tx| {
        tx.execute_batch(
            "DROP TABLE task_result_settlements; DROP TABLE tasks; DROP TABLE messages;",
        )?;
        Ok(())
    })?;
    let selected = current.capture(&request().capture)?;
    assert_eq!(
        selected
            .output_source("output-task")?
            .unwrap()
            .message
            .key
            .id,
        "current-output-source"
    );
    let task = selected
        .lookup(&TaskKey::new(TaskFamily::Task, "output-task"))?
        .unwrap();
    assert!(
        matches!(&task.fact,CanonicalTaskFact::Task(task) if task.status == crate::types::TaskStatus::Completed)
    );
    assert_eq!(
        selected
            .coverage
            .output
            .window
            .as_ref()
            .unwrap()
            .message_ids,
        vec!["current-output-source"]
    );
    drop((selected, current, task_ack, after_source, ack, before));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn output_source_status_follows_received_metadata_and_never_raw_updates() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        task_locator(tx)?;
        message(
            tx,
            "status-only-source",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: "output-task".into(),
            },
            Some("output-task"),
            None,
        )?;
        let current =
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
                .require_current()?
                .resident
                .capture(&request().capture)?;
        assert_eq!(
            current
                .output_source("output-task")?
                .unwrap()
                .message_status,
            crate::types::TaskStatus::Running
        );
        let mut changed = crate::runtime_db::evidence::committed_message_tx(
            tx,
            &scope().agent_id,
            "status-only-source",
        )?;
        changed.metadata.as_mut().unwrap()["task_status"] = serde_json::json!("failed");
        crate::runtime_db::evidence::upsert_message_tx(tx, &changed)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(changed.clone())],
        )?;
        let current =
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
                .require_current()?
                .resident;
        assert_eq!(
            current
                .capture(&request().capture)?
                .output_source("output-task")?
                .unwrap()
                .message_status,
            crate::types::TaskStatus::Failed
        );
        changed.metadata.as_mut().unwrap()["task_status"] = serde_json::json!("completed");
        crate::runtime_db::evidence::upsert_message_tx(tx, &changed)?;
        let observed =
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
                .require_current()?
                .resident
                .capture(&request().capture)?;
        assert_eq!(
            observed
                .output_source("output-task")?
                .unwrap()
                .message_status,
            crate::types::TaskStatus::Failed,
            "unreceived raw metadata cannot change the accepted status"
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn fixed_output_source_status_is_original_metadata_without_body_alias() -> Result<()> {
    let (_dir, db) = crate::runtime_db::task_snapshot::tests::legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &task("output-task", crate::types::TaskStatus::Running))?;
        let mut source = MessageEnvelope::new(
            &scope().agent_id,
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: "output-task".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original cancelling".into(),
            },
        );
        source.id = "fixed-status-source".into();
        source.created_at = at();
        source.metadata =
            Some(serde_json::json!({"task_id":"output-task","task_status":"cancelling"}));
        append_message_tx(tx, &source)?;
        let mut state = crate::types::AgentState::new(&scope().agent_id);
        state.total_message_count = 1;
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &state)
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let observed=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&request(),&ledger)?
            .require_current()?.resident.capture(&request().capture)?;
        assert_eq!(observed.output_source("output-task")?.unwrap().message_status,crate::types::TaskStatus::Cancelling);
        let aliases:u64=tx.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE family='message' AND record_id='fixed-status-source'",[],|row|row.get(0))?;
        assert_eq!(aliases,0,"accepted status metadata does not grant a Source body");
        Ok(())
    })
}

#[test]
fn output_status_scalar_budget_precedes_damaged_enum_decode() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let task = task_locator(tx)?;
        message(
            tx,
            "status-budget-source",
            MessageKind::TaskStatus,
            MessageOrigin::Task {
                task_id: "output-task".into(),
            },
            Some("output-task"),
            None,
        )?;
        tx.execute_batch("PRAGMA ignore_check_constraints=ON")?;
        let changed = tx.execute(
            "UPDATE task_message_sources SET message_status=?1",
            ["invalid".repeat(100_000)],
        )?;
        tx.execute_batch("PRAGMA ignore_check_constraints=OFF")?;
        assert_eq!(changed, 1);
        let mut guards = Vec::new();
        let error = observe_output_heads_tx(tx, &scope(), &[&task], |bytes, _| {
            anyhow::ensure!(
                bytes <= 64 * 1024,
                TaskSnapshotError::ResourceExhausted("Source status material")
            );
            guards.push(ledger.register_owned_bytes(
                &scope(),
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: 0,
                    metadata_estimate_bytes: u64::try_from(bytes)?,
                },
            )?);
            Ok(())
        })
        .err()
        .context("oversize damaged status accepted")?;
        assert!(
            matches!(
                error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        drop(guards);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn repeated_fixed_source_preparation_does_not_read_the_original_message_inventory() -> Result<()> {
    let (_dir, db) = crate::runtime_db::task_snapshot::tests::legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &task("output-task", crate::types::TaskStatus::Running))?;
        let mut source = MessageEnvelope::new(
            &scope().agent_id,
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: "output-task".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original source".into(),
            },
        );
        source.id = "once-fixed-source".into();
        source.created_at = at();
        source.metadata = Some(serde_json::json!({"task_id":"output-task"}));
        append_message_tx(tx, &source)?;
        let mut state = crate::types::AgentState::new(&scope().agent_id);
        state.total_message_count = 1;
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &state)
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let initial=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&request(),&ledger)?
            .require_current()?.resident.capture(&request().capture)?;
        assert_eq!(initial.output_source("output-task")?.unwrap().message.key.id,"once-fixed-source");
        let seed: String = tx.query_row(
            "SELECT seed_id FROM task_message_source_inventory WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
            |row| row.get(0),
        )?;
        let fixed = crate::runtime_db::episode_snapshot::task_checkpoint::load(tx, &seed, "upgrade")?;
        // Deny only the initializer's original bulk input. Finite availability
        // validation below still reads the intact fixed member table.
        tx.execute_batch("ALTER TABLE task_query_fixed_messages RENAME TO once_unavailable_fixed_messages")?;
        super::initialize_source_inventory_tx(tx, &scope(), Some(&seed), &[fixed])?;
        tx.execute_batch("ALTER TABLE once_unavailable_fixed_messages RENAME TO task_query_fixed_messages")?;
        let repeated=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&request(),&ledger)?
            .require_current()?.resident.capture(&request().capture)?;
        assert_eq!(repeated.output_source("output-task")?,initial.output_source("output-task")?);
        assert_eq!(repeated.coverage.output,initial.coverage.output);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
