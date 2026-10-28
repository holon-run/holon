use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::resident_task::{TaskCaptureRequest, TaskKey},
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        input_mutation_receipts::{
            insert_input_mutation_receipt_tx, InputMutationOutcome, InputMutationScopeOutcome,
        },
        repositories,
        task_snapshot::{
            prepare_task_domain_snapshot_tx, prepare_task_write_tx, register_task_writes_tx,
            TaskDomainLoadRequest,
        },
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility,
    },
};
use std::collections::BTreeSet;

fn at() -> DateTime<Utc> {
    "2026-10-01T00:00:00.123Z".parse().unwrap()
}
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "stop-agent".into(),
        identity_incarnation: 1,
    }
}
fn request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn task() -> TaskRecord {
    TaskRecord {
        id: "original-task".into(),
        agent_id: scope().agent_id,
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: at(),
        updated_at: at(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("running".into()),
        detail: Some(serde_json::json!({"cancel_requested":false})),
        recovery: None,
    }
}
fn fixture() -> Result<(tempfile::TempDir, RuntimeDb, InputEvidenceBudgetLedger)> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                "stop-agent",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new("stop-agent"))?;
        prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "original-task")]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &task())?;
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("stop-agent"),
            &request(),
        )?;
        Ok(())
    })?;
    Ok((
        dir,
        db,
        InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?,
    ))
}
fn stop(
    tx: &Transaction<'_>,
    ledger: &InputEvidenceBudgetLedger,
    nonce: &str,
    signal: TaskStopSignal,
    write_receipt: bool,
) -> Result<TaskStopRecord> {
    let scope = scope();
    let request = request();
    let key = TaskKey::new(TaskFamily::Task, "original-task");
    let snapshot = crate::runtime_db::task_snapshot::prepare_task_domain_snapshot_with_budget_tx(
        tx, &scope, &request, ledger,
    )?
    .require_current()?
    .resident;
    let capture = snapshot.capture(&request.capture)?;
    let locator = capture
        .lookup(&key)?
        .context("Task fixture missing")?
        .locator
        .clone();
    let original = prepare_task_write_tx(tx, &scope, &BTreeSet::from([key.clone()]))?;
    let intent = PreparedTaskStop {
        task: locator,
        kind: TaskKind::CommandTask,
        created_at: at(),
        rejoin: None,
        owner_epoch: 1,
        signal,
        lease_id: Some("original-lease".into()),
        child: None,
        child_unavailable: None,
        worktree: None,
        worktree_unavailable: None,
        result: None,
    };
    let prepared = PreparedInputMutation::new(
        InputMutationFamily::Task,
        nonce.into(),
        vec![scope.clone()],
        canonical_command_digest(&intent)?,
        at(),
    )?;
    let proof = prepare_task_stop_tx(tx, &prepared, &intent, &original, &request, ledger)?;
    let mut expected = original.task_before("original-task")?.clone();
    expected.status = TaskStatus::Cancelling;
    expected.detail.as_mut().unwrap()["cancel_requested"] = serde_json::json!(true);
    if signal == TaskStopSignal::Force {
        expected.detail.as_mut().unwrap()["force_stop_requested"] = serde_json::json!(true);
    }
    repositories::upsert_task_tx(tx, &expected)?;
    let (result, inserted) = insert_task_stop_with_observation_ack_tx(&proof, &expected)?;
    crate::runtime_db::task_snapshot::register_task_writes_with_stop_observation_tx(
        tx,
        &original,
        &BTreeSet::from([key]),
        &AgentState::new("stop-agent"),
        &request,
        ledger,
        None,
        &inserted,
    )?;
    if write_receipt {
        let outcome = InputMutationOutcome {
            result: InputMutationResult::Task {
                task_id: "original-task".into(),
                deletion: None,
                stop: Some(result.receipt_binding()),
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
        };
        insert_input_mutation_receipt_tx(tx, &prepared, &outcome, at())?;
    }
    Ok(result)
}

#[test]
fn same_clock_force_follows_cancel_by_allocated_sequence_and_original_receipt() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let cancel =
        db.transaction(|tx| stop(tx, &ledger, "z-cancel", TaskStopSignal::Cancel, true))?;
    let force = db.transaction(|tx| stop(tx, &ledger, "a-force", TaskStopSignal::Force, true))?;
    assert!(force.effect_seq > cancel.effect_seq);
    assert_eq!(force.generation_digest, cancel.generation_digest);
    db.transaction(|tx| {
        let head:String=tx.query_row("SELECT command_nonce FROM task_stop_effects WHERE generation_digest=?1 ORDER BY effect_seq DESC LIMIT 1",[&force.generation_digest],|r|r.get(0))?;
        assert_eq!(head,"a-force");
        let _receipt=validate_original_task_stop_receipt_tx(tx,&cancel,4*1024*1024,&ledger)?;
        let pending=load_pending_task_stops_tx(tx,&scope(),100,4*1024*1024,&ledger)?;
        assert_eq!(pending.records.len(),2);
        assert_eq!(pending.records[0].effect_seq,cancel.effect_seq);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_receipt_is_required_in_the_same_transaction() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    assert!(db
        .transaction(|tx| stop(tx, &ledger, "no-receipt", TaskStopSignal::Cancel, false))
        .is_err());
    db.transaction(|tx| {
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM task_stop_effects", [], |r| r
                .get::<_, usize>(0))?,
            0
        );
        let raw: String = tx.query_row(
            "SELECT payload_json FROM tasks WHERE task_id='original-task'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(
            repositories::decode_task_payload(&raw)?.status,
            TaskStatus::Running
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn pending_normalized_material_is_budgeted_before_corrupt_body_decode() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| stop(tx, &ledger, "oversized", TaskStopSignal::Cancel, true))?;
    let error=db.transaction(|tx| {
        tx.execute("UPDATE task_stop_effects SET task_id=?1,intent_json='invalid-json' WHERE command_nonce='oversized'",["x".repeat(65536)])?;
        load_pending_task_stops_tx(tx,&scope(),100,4096,&ledger)
    }).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ResourceExhausted before decode"),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn normalized_pending_responsibility_cannot_be_hidden_by_payload_absence() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let mut record =
        db.transaction(|tx| stop(tx, &ledger, "attention", TaskStopSignal::Cancel, true))?;
    // Construct a malformed normalized projection instead of silently dropping
    // an independently pending responsibility from the complete inventory.
    record.progress.physical = TaskStopPhysicalState::NeedsAttention;
    db.transaction(|tx| {
        tx.execute("UPDATE task_stop_effects SET physical_state='needs_attention',result_state='pending',progress_json=?1 WHERE command_nonce='attention'",[serde_json::to_string(&record.progress)?])?;
        let error=load_pending_task_stops_tx(tx,&scope(),100,4*1024*1024,&ledger).unwrap_err();
        assert!(error.to_string().contains("projection differs"),"{error:#}");
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn pending_receipt_material_is_reserved_before_corrupt_outcome_decode() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| stop(tx, &ledger, "receipt-bytes", TaskStopSignal::Cancel, true))?;
    db.transaction(|tx| {
        let baseline = load_pending_task_stops_tx(tx, &scope(), 100, 65_536, &ledger)?;
        assert_eq!(baseline.records.len(), 1);
        drop(baseline);
        tx.execute("UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce='receipt-bytes'",
            ["invalid-json".repeat(10_000)])?;
        let error = load_pending_task_stops_tx(tx, &scope(), 100, 65_536, &ledger).unwrap_err();
        assert!(error.to_string().contains("ResourceExhausted before decode"), "{error:#}");
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn selected_stop_pins_share_allocations_without_retaining_the_inventory() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| stop(tx, &ledger, "first", TaskStopSignal::Cancel, true))?;
    db.transaction(|tx| stop(tx, &ledger, "second", TaskStopSignal::Force, true))?;
    let inventory = db.transaction(|tx| {
        load_pending_task_stops_tx(tx, &scope(), 100, 4 * 1024 * 1024, &ledger)
    })?;
    let current = ledger.register_task_stop_view(
        &scope(),
        InputMemoryCategory::Current,
        &inventory.records,
    )?;
    let selected = inventory.records[0].clone();
    let pins = ledger.register_task_stop_view(&scope(), InputMemoryCategory::Pins, [&selected])?;
    assert_eq!(ledger.report()?.physical_records.allocations, 2);
    drop(current);
    drop(inventory);
    assert_eq!(ledger.report()?.physical_records.allocations, 1);
    assert_eq!(ledger.report()?.owned.payload_bytes, 0);
    drop(selected);
    drop(pins);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn stale_owned_proof_cannot_authorize_deleted_successor_or_new_deleting_scope() -> Result<()> {
    for (status, incarnation) in [("deleted", 1), ("active", 2), ("deleting", 1)] {
        let (_dir, db, ledger) = fixture()?;
        let scope = scope();
        let request = request();
        let key = TaskKey::new(TaskFamily::Task, "original-task");
        let (original, locator) = db.transaction(|tx| {
            let domain = prepare_task_domain_snapshot_tx(tx, &scope, &request)?
                .require_current()?
                .resident;
            let captured = domain.capture(&request.capture)?;
            let locator = captured
                .lookup(&key)?
                .context("original Task missing")?
                .locator
                .clone();
            Ok((
                prepare_task_write_tx(tx, &scope, &BTreeSet::from([key.clone()]))?,
                locator,
            ))
        })?;
        db.transaction(|tx| {
            tx.execute("UPDATE agent_identities SET status=?1,payload_json=json_set(payload_json,'$.status',?1,'$.incarnation',?2) WHERE agent_id='stop-agent'",
                params![status,incarnation])?;
            // A retired-proof rejection cannot depend on reading another State.
            tx.execute_batch("ALTER TABLE agent_states RENAME TO hidden_successor_states")?;
            Ok(())
        })?;
        let intent = PreparedTaskStop {
            task: locator,
            kind: TaskKind::CommandTask,
            created_at: at(),
            rejoin: None,
            owner_epoch: 1,
            signal: TaskStopSignal::Cancel,
            lease_id: Some("original-lease".into()),
            child: None,
            child_unavailable: None,
            worktree: None,
            worktree_unavailable: None,
            result: None,
        };
        let prepared = PreparedInputMutation::new(
            InputMutationFamily::Task,
            "stale-stop".into(),
            vec![scope.clone()],
            canonical_command_digest(&intent)?,
            at(),
        )?;
        db.transaction(|tx| {
            let error=prepare_task_stop_tx(tx,&prepared,&intent,&original,&request,&ledger).err().context("stale proof unexpectedly authorized Stop")?;
            let expected = match (status, incarnation) {
                ("deleted", _) => "Task maintenance scope retired",
                ("active", 2) => "Task maintenance identity projection/incarnation mismatch",
                _ => "new TaskStop requires the original Active scope",
            };
            assert!(error.to_string().contains(expected), "{error:#}");
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_stop_effects",[],|row|row.get::<_,usize>(0))?,0);
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM tasks WHERE task_id='original-task'",[],|row|row.get::<_,usize>(0))?,1);
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_domain_records WHERE record_id='original-task' AND availability='present'",[],|row|row.get::<_,usize>(0))?,1);
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn original_monitor_join_updates_only_focused_progress_and_replays_exactly() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    let original =
        db.transaction(|tx| stop(tx, &ledger, "joined", TaskStopSignal::Cancel, true))?;
    let wrong = crate::runtime::task_stop_join::join_fixture(&original, true, &ledger).await;
    assert!(
        format!("{:#}", wrong.err().context("foreign lease was accepted")?)
            .contains("frozen original lease")
    );
    let joined = crate::runtime::task_stop_join::join_fixture(&original, false, &ledger).await?;
    let join_at = at() + chrono::Duration::nanoseconds(456789);
    let canonical = db.transaction(|tx| {
        tx.query_row(
            "SELECT payload_json FROM tasks WHERE task_id='original-task'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(Into::into)
    })?;
    db.transaction(|tx| {
        let changed =
            mark_task_stop_monitor_joined_tx(tx, &original, &joined, join_at, MAX_BYTES, &ledger)?;
        assert!(changed.changed);
        assert_eq!(changed.record.updated_at, at());
        assert_eq!(changed.record.revision, original.revision + 1);
        assert_eq!(
            changed.record.progress.physical,
            TaskStopPhysicalState::MonitorJoined
        );
        assert_eq!(
            changed.record.progress.result,
            TaskStopResultState::NotRequired
        );
        let replay =
            mark_task_stop_monitor_joined_tx(tx, &original, &joined, join_at, MAX_BYTES, &ledger)?;
        assert!(!replay.changed);
        assert_eq!(replay.record, changed.record);
        assert!(
            load_pending_task_stops_tx(tx, &scope(), MAX_RECORDS, MAX_BYTES, &ledger)?
                .records
                .is_empty()
        );
        assert_eq!(
            load_task_stop_by_sequence_tx(tx, &scope(), original.effect_seq, MAX_BYTES, &ledger)?
                .records
                .len(),
            1
        );
        assert_eq!(
            canonical,
            tx.query_row(
                "SELECT payload_json FROM tasks WHERE task_id='original-task'",
                [],
                |row| row.get::<_, String>(0)
            )?
        );
        Ok(())
    })?;
    drop(joined);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn original_monitor_join_rejects_changed_progress_and_corrupt_original_receipt() -> Result<()>
{
    for corrupt_receipt in [false, true] {
        let (_dir, db, ledger) = fixture()?;
        let original = db
            .transaction(|tx| stop(tx, &ledger, "joined-conflict", TaskStopSignal::Cancel, true))?;
        let joined =
            crate::runtime::task_stop_join::join_fixture(&original, false, &ledger).await?;
        db.transaction(|tx| {
            if corrupt_receipt {
                tx.execute("UPDATE input_mutation_receipts SET result_digest=printf('%064d',0) WHERE command_nonce='joined-conflict'", [])?;
            } else {
                tx.execute("UPDATE task_stop_effects SET revision=revision+1 WHERE command_nonce='joined-conflict'", [])?;
            }
            Ok(())
        })?;
        let error = db
            .transaction(|tx| {
                mark_task_stop_monitor_joined_tx(tx, &original, &joined, at(), MAX_BYTES, &ledger)
                    .map(|_| ())
            })
            .unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.contains(if corrupt_receipt {
                "checksum"
            } else {
                "revision conflict"
            }),
            "{text}"
        );
        drop(joined);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[path = "resident_tests.rs"]
mod resident_tests;

#[path = "result_receipt_budget_tests.rs"]
mod result_receipt_budget_tests;

#[path = "scope_inventory_tests.rs"]
mod scope_inventory_tests;
