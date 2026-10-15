use super::super::writes::prepare_original_state_source_tx;
use super::*;
use crate::runtime_db::{
    task_snapshot::TaskDomainLoadRequest,
    transitions::{
        AgentPostureTransitionCommand, AgentStateMutation, InputMutationExecution,
        PreparedInputTransition,
    },
    work_snapshot::WorkDomainLoadRequest,
};
fn proof_request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        byte_limit: CURRENT_READ_MAX_BYTES,
        record_limit: CURRENT_READ_MAX_RECORDS,
    }
}
fn task_request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        byte_limit: CURRENT_READ_MAX_BYTES,
        record_limit: CURRENT_READ_MAX_RECORDS,
    }
}
#[test]
fn original_state_source_shares_bodies_without_queue_event_sql_and_no_guard_chain() -> Result<()> {
    let (_home, db, before) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &before, &request(), &budget)
    })?;
    let source = old.resident.clone();
    let old_source = Arc::downgrade(&source);
    let selected = source.capture(&request())?;
    let old_event = selected.events[0].clone();
    let body = Arc::downgrade(&old_event);
    let physical = budget.report()?.physical_records.bytes;
    db.transaction(|tx|{tx.execute_batch("ALTER TABLE queue_entries RENAME TO source_queue_forbidden; ALTER TABLE audit_events RENAME TO source_events_forbidden")?;Ok(())})?;
    let mut after = before.clone();
    after.total_message_count += 1;
    let facts = db.transaction(|tx| {
        let proof = prepare_original_state_source_tx(
            tx,
            &scope(),
            &before,
            &request(),
            &budget,
            (0, 0),
            Vec::new(),
            source.clone(),
        )?;
        upsert_agent_state_tx(tx, &after)?;
        register_current_read_writes_tx(proof, &[], &after)
    })?;
    assert_eq!(facts.revision, source.coverage().revision + 1);
    assert_eq!(
        facts.snapshot.resident.coverage().events,
        source.coverage().events
    );
    let capture = facts.snapshot.resident.capture(&request())?;
    assert!(Arc::ptr_eq(&capture.events[0], &old_event));
    assert_eq!(budget.report()?.physical_records.bytes, physical);
    drop((old, source, selected, old_event));
    assert!(
        old_source.upgrade().is_none(),
        "new root must not retain the original root/metadata source"
    );
    assert!(
        body.upgrade().is_some(),
        "new root shares the exact qualified body allocation"
    );
    // An original no-op keeps its exact revision and still shares all bodies.
    let unchanged = db.transaction(|tx| {
        let proof = prepare_original_state_source_tx(
            tx,
            &scope(),
            &after,
            &request(),
            &budget,
            (0, 0),
            Vec::new(),
            facts.snapshot.resident.clone(),
        )?;
        register_current_read_writes_tx(proof, &[], &after)
    })?;
    assert_eq!(unchanged.revision, facts.revision);
    drop((facts, capture, unchanged));
    assert!(body.upgrade().is_none());
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn state_source_rejects_raw_record_drift_wrong_ledger_and_successor_before_state_decode(
) -> Result<()> {
    let (_home, db, before) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &before, &request(), &budget)
    })?;
    let foreign = ledger()?;
    assert!(db
        .transaction(|tx| prepare_original_state_source_tx(
            tx,
            &scope(),
            &before,
            &request(),
            &foreign,
            (0, 0),
            Vec::new(),
            old.resident.clone()
        )
        .map(|_| ()))
        .is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        let mut proof = prepare_original_state_source_tx(
            tx,
            &scope(),
            &before,
            &request(),
            &budget,
            (0, 0),
            Vec::new(),
            old.resident.clone(),
        )?;
        let original = QueueEntryRecord {
            message_id: "original-current-read-input".into(),
            agent_id: scope().agent_id,
            priority: Priority::Next,
            status: QueueEntryStatus::Queued,
            created_at: at(),
            updated_at: at(),
        };
        assert!(
            upsert_current_read_queue_tx(&mut proof, &original).is_err(),
            "State-only source grants no original Queue operation"
        );
        assert!(
            append_current_read_event_tx(
                &mut proof,
                &AuditEvent::legacy("forbidden", serde_json::json!({}))
            )
            .is_err(),
            "State-only source grants no Audit append"
        );
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM audit_events", [], |r| r
                .get::<_, usize>(0))?,
            1
        );
        Ok(())
    })?;
    let saved: String =
        db.connection()?
            .query_row("SELECT coverage_json FROM current_read_current", [], |r| {
                r.get(0)
            })?;
    let rollback:Result<()>=db.transaction(|tx|{
        tx.execute("UPDATE queue_entries SET priority='raw-tamper' WHERE message_id='original-current-read-input'",[])?;
        let error=prepare_original_state_source_tx(tx,&scope(),&before,&request(),&budget,(0,0),Vec::new(),old.resident.clone()).err().context("raw record drift invalidates the original source")?;
        assert!(matches!(error.downcast_ref::<CurrentReadError>(),Some(CurrentReadError::NeedsPreparation)));
        let mut after=before.clone();after.total_message_count+=1;upsert_agent_state_tx(tx,&after)?;
        assert_eq!(tx.query_row("SELECT freshness FROM current_read_current",[],|r|r.get::<_,String>(0))?,"records_invalidated");
        anyhow::bail!("restore deliberate raw fixture drift")
    });
    assert!(rollback.is_err());
    assert_eq!(
        db.connection()?
            .query_row("SELECT coverage_json FROM current_read_current", [], |r| {
                r.get::<_, String>(0)
            })?,
        saved
    );
    let hidden: Result<()> = db.transaction(|tx| {
        let proof = prepare_original_state_source_tx(
            tx,
            &scope(),
            &before,
            &request(),
            &budget,
            (0, 0),
            Vec::new(),
            old.resident.clone(),
        )?;
        let event =
            AuditEvent::legacy("hidden-original-append", serde_json::json!({"actual":true}));
        super::super::super::evidence::append_audit_event_tx(tx, Some(&scope().agent_id), &event)?;
        let error = register_current_read_writes_tx(proof, &[], &before)
            .err()
            .context("State-only source cannot conceal an actual Audit append")?;
        assert!(matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::RevisionMismatch)
        ));
        anyhow::bail!("rollback hidden original Event responsibility")
    });
    assert!(hidden.is_err());
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM audit_events", [], |r| r
                .get::<_, usize>(0))?,
        1
    );
    db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2)",
            [],
        )?;
        tx.execute(
            "UPDATE agent_states SET payload_json=?1",
            ["{not-json".repeat(8192)],
        )?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            prepare_original_state_source_tx(
                tx,
                &scope(),
                &before,
                &request(),
                &budget,
                (0, 0),
                Vec::new(),
                old.resident.clone(),
            )
            .map(|_| ())
        })
        .err()
        .context("retired original source must reject before successor State")?;
    assert!(matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::RetiredScope)
    ));
    drop(old);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn state_source_combined_allowance_counts_original_shared_material_before_new_decode() -> Result<()>
{
    let (_home, db, before) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &before, &request(), &budget)
    })?;
    let shared = old.resident.material_estimate()?;
    let baseline = budget.report()?.observed_bytes;
    let mut small = request();
    small.byte_limit = shared.0 + 1024;
    small.record_limit = shared.1 + 16;
    small.check_material(shared.0, shared.1)?;
    let error = db
        .transaction(|tx| {
            prepare_original_state_source_tx(
                tx,
                &scope(),
                &before,
                &small,
                &budget,
                (0, 0),
                Vec::new(),
                old.resident.clone(),
            )
            .map(|_| ())
        })
        .err()
        .context("shared plus new phase must exceed its combined allowance")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, baseline);
    assert_eq!(
        db.connection()?
            .query_row("SELECT revision FROM current_read_current", [], |r| r
                .get::<_, u64>(0))?,
        old.resident.coverage().revision
    );
    drop(old);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn actual_prepared_state_source_ack_rollback_and_exact_replay_keep_original_digest() -> Result<()> {
    let (_home, db, before) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &before, &request(), &budget)
    })?;
    let mut after = before.clone();
    after.total_message_count += 1;
    let command = PreparedInputTransition::posture(
        scope(),
        "pure-current-state".into(),
        at(),
        AgentPostureTransitionCommand {
            agent_id: scope().agent_id,
            expected_incarnation: Some(1),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(before.clone())),
                record: Box::new(after.clone()),
            },
            audit_events: Vec::new(),
            fault: None,
        },
        None,
    )?;
    let descriptor = command.descriptor().clone();
    let prepared = command.with_current_read_source(old.resident.clone())?;
    assert_eq!(prepared.descriptor(), &descriptor);
    db.connection()?.execute_batch("CREATE TRIGGER fail_pure_state_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='pure-current-state' BEGIN SELECT RAISE(ABORT,'pure original receipt rollback'); END;")?;
    let commit = || {
        db.transitions_in_scope(&scope())
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &prepared,
                &proof_request(),
                &budget,
                &task_request(),
            )
    };
    let error = commit()
        .err()
        .context("original receipt failure must roll back State and Current")?;
    assert!(
        format!("{error:#}").contains("pure original receipt rollback"),
        "{error:#}"
    );
    assert_eq!(db.agent_states().latest(&scope().agent_id)?, Some(before));
    assert_eq!(
        db.connection()?
            .query_row("SELECT revision FROM current_read_current", [], |r| r
                .get::<_, u64>(0))?,
        old.resident.coverage().revision
    );
    db.connection()?
        .execute_batch("DROP TRIGGER fail_pure_state_receipt")?;
    let InputMutationExecution::Committed { value, receipt } = commit()? else {
        unreachable!()
    };
    let current = value
        .effects
        .current_read_facts
        .as_ref()
        .context("actual Current source ACK missing")?;
    assert_eq!(current.revision, old.resident.coverage().revision + 1);
    assert_eq!(db.agent_states().latest(&scope().agent_id)?, Some(after));
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = commit()?
    else {
        unreachable!()
    };
    assert_eq!(receipt, replay);
    assert_eq!(
        db.connection()?
            .query_row("SELECT revision FROM current_read_current", [], |r| r
                .get::<_, u64>(0))?,
        current.revision
    );
    drop((old, prepared, value, receipt, replay));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_missing_history_table_upgrade_remains_unavailable_with_state_only_markers() -> Result<()>
{
    let home = tempfile::tempdir()?;
    let path = home.path().join("missing-history.sqlite");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for m in MIGRATIONS.iter().filter(|m| m.version <= 81) {
        apply_migration(&mut connection, m)?;
    }
    super::super::super::ensure_runtime_identity_metadata(&connection)?;
    let state = AgentState::new(&scope().agent_id);
    {
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(
            &tx,
            &AgentIdentityRecord::new(
                scope().agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(&tx, &state)?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[repositories::timestamp(at())])?;
        tx.commit()?;
    }
    for m in MIGRATIONS
        .iter()
        .filter(|m| m.version > 81 && m.version < 90)
    {
        apply_migration(&mut connection, m)?;
    }
    connection.execute_batch("ALTER TABLE queue_entries RENAME TO absent_original_queue; ALTER TABLE audit_events RENAME TO absent_original_events")?;
    let original = MIGRATIONS.iter().find(|m| m.version == 90).unwrap();
    apply_migration(&mut connection, original)?;
    assert_eq!(
        connection.query_row(
            "SELECT queue_available,event_available FROM current_read_fixed_captures",
            [],
            |r| Ok((r.get::<_, bool>(0)?, r.get::<_, bool>(1)?))
        )?,
        (false, false)
    );
    connection.execute_batch("ALTER TABLE absent_original_queue RENAME TO queue_entries; ALTER TABLE absent_original_events RENAME TO audit_events")?;
    drop(connection);
    let db = RuntimeDb::open_and_migrate(path, home.path().join("runtime.lock"))?;
    let budget = ledger()?;
    let error = db
        .transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
        })
        .err()
        .context("late empty tables cannot replace original Unavailable history")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::HistoricalUnavailable)
        ),
        "{error:#}"
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
