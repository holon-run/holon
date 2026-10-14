use super::*;
use crate::{
    domain::execution_protocol::{ExecutionAttempt, ExecutionProtocolCommand},
    runtime_db::{
        input_evidence_snapshot::{
            legacy_recovery::ExclusiveStartupInputRecovery, register_committed_input_evidence_tx,
            CanonicalInputEvidence,
        },
        task_result_settlement::{self as results, TaskResultWriteLimits},
        transitions::{
            AgentStateMutation, ExecutionProtocolTransition, PreparedInputTransition,
            PreparedQueueTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
            StartupInputRecoveryScope,
        },
        RuntimeDbLock,
    },
    types::{
        AdmissionContext, AgentStatus, MessageDeliverySurface, QueueEntryRecord, QueueEntryStatus,
    },
};

fn seed(
    db: &RuntimeDb,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(ResidentTaskDomain, String)> {
    let mut source = MessageEnvelope::new(
        "task-agent",
        MessageKind::Control,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original startup source".into(),
        },
    );
    source.id = "startup-task-source".into();
    source.created_at = at();
    let queued = QueueEntryRecord {
        message_id: source.id.clone(),
        agent_id: source.agent_id.clone(),
        priority: source.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: at(),
        updated_at: at(),
    };
    let before = db
        .agent_states()
        .latest("task-agent")?
        .context("original State missing")?;
    let mut pending = before.clone();
    pending.pending = 1;
    let queue_command = |mutation, state, evidence| QueueTransitionCommand {
        agent_id: "task-agent".into(),
        operation: QueueOperation::Admit,
        mutation,
        scheduler_claim_work_item: None,
        agent_state: state,
        message_evidence: evidence,
        transcript_entries: vec![],
        turn_record: None,
        audit_events: vec![],
        notify_scheduler: false,
        fault: None,
        brief_evidence: vec![],
    };
    let admit = PreparedInputTransition::queue(
        scope(),
        "startup-source-admit".into(),
        at(),
        PreparedQueueTransition::new(queue_command(
            QueueMutation::Upsert(queued.clone()),
            Some(AgentStateMutation {
                expected: Some(Box::new(before)),
                record: Box::new(pending),
            }),
            vec![source],
        )),
    )?;
    db.transitions_in_scope(&scope())
        .commit_prepared_transition(&admit)?;
    let source = db
        .messages()
        .by_id(Some("task-agent"), "startup-task-source")?
        .context("actual admitted source missing")?;
    let before = db
        .agent_states()
        .latest("task-agent")?
        .context("pending State missing")?;
    let mut running = before.clone();
    running.pending = 0;
    running.status = AgentStatus::AwakeRunning;
    running.current_run_id = Some("startup-task-run".into());
    let control_revision: u64 = db.connection()?.query_row(
        "SELECT control_revision FROM agent_states WHERE agent_id='task-agent'",
        [],
        |r| r.get(0),
    )?;
    let attempt = ExecutionAttempt {
        attempt_id: "startup-task-attempt".into(),
        agent_id: "task-agent".into(),
        source_message_id: Some(source.id.clone()),
        source: ExecutionSource {
            identity: ExecutionSourceIdentity::QueueMessage {
                message_id: source.id.clone(),
            },
            generation: source.message_seq.unwrap(),
        },
        binding: ExecutionBinding::Conversation {
            interaction_id: source.id.clone(),
        },
        provenance: ExecutionProvenance {
            origin: ExecutionOrigin::Operator,
            trust: ExecutionTrust::OperatorInstruction,
            priority: ExecutionPriority::Normal,
            correlation_id: None,
            causation_id: None,
        },
        admitted_fences: AdmittedFences {
            source_revision: source.message_seq.unwrap(),
            work_item_source_revision: None,
            work_item_generation: None,
            rejoin: None,
            agent_control_revision: control_revision,
            host_registry_revision: 1,
        },
        state: ExecutionAttemptState::Open,
        run_id: None,
        turn_id: None,
        recovery_of_attempt_id: None,
        terminal_outcome_id: None,
        admitted_at: at().to_rfc3339(),
        terminal_at: None,
    };
    let mut dequeued = queued;
    dequeued.status = QueueEntryStatus::Dequeued;
    let mut claim = PreparedQueueTransition::new(queue_command(
        QueueMutation::Consume(dequeued),
        Some(AgentStateMutation {
            expected: Some(Box::new(before)),
            record: Box::new(running),
        }),
        vec![],
    ));
    claim.command.operation = QueueOperation::Claim;
    claim.execution_protocol = ExecutionProtocolTransition {
        bootstrap: Some(ExecutionProtocolState::empty("task-agent")),
        commands: vec![ExecutionProtocolCommand::Admit(Box::new(AdmitExecution {
            attempt,
        }))],
    };
    let claim =
        PreparedInputTransition::queue(scope(), "startup-source-claim".into(), at(), claim)?;
    db.transitions_in_scope(&scope())
        .commit_prepared_transition(&claim)?;
    db.transaction(|tx| {
        let mut terminal = task("startup-metadata-task", TaskStatus::Completed);
        terminal.parent_message_id = Some("startup-metadata-result".into());
        terminal.detail = Some(serde_json::json!({"rejoin_obligation_id":terminal.id,"rejoin_generation":1,"parent_turn_id":"original-parent"}));
        let mut message = MessageEnvelope::new("task-agent", MessageKind::TaskResult,
            MessageOrigin::Task { task_id: terminal.id.clone() }, AuthorityClass::RuntimeInstruction,
            Priority::Next, MessageBody::Text { text: "completed original Task".into() });
        message = message.with_admission(MessageDeliverySurface::TaskRejoin, AdmissionContext::RuntimeOwned);
        message.id = terminal.parent_message_id.clone().unwrap(); message.task_id = Some(terminal.id.clone()); message.created_at = at();
        let result = TaskResultSettlementRecord::pending(&terminal, &message, at())?.context("pending Result missing")?;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task,&terminal.id), TaskKey::new(TaskFamily::ResultSettlement,&result.result_identity)]);
        let proof = prepare_task_write_tx(tx,&scope(),&keys)?;
        repositories::upsert_task_tx(tx,&terminal)?;
        let (message,_) = append_message_tx(tx,&message)?;
        register_committed_input_evidence_tx(tx,&scope(),&[CanonicalInputEvidence::Message(message.clone())])?;
        results::prepare_upsert_pending_tx(tx,&terminal,&result,TaskResultWriteLimits::default())?.execute()?;
        results::prepare_admit_message_tx(tx,"task-agent",&message.id,"startup-task-attempt",at(),TaskResultWriteLimits::default())?.execute()?;
        let actual_state = crate::runtime_db::context_inputs::ContextDurableRead::new(tx,"task-agent").scope_facts()?.state.context("claimed State missing")?;
        register_task_writes_with_budget_tx(tx,&proof,&keys,&actual_state,&request(),ledger)?;
        let domain = prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&request(),ledger)?.require_current()?.resident;
        Ok((domain,result.result_identity))
    })
}
fn recover(
    db: &RuntimeDb,
    lease: &RuntimeDbLock,
    path: &std::path::Path,
    guard: &tokio::sync::MutexGuard<'_, ()>,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<()> {
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(db, lease, path, guard)?;
    let scopes = vec![StartupInputRecoveryScope::Current(scope())];
    db.recover_interrupted_runtime_state_at_startup_with_tasks_in_scopes(&scopes, &cap, ledger)?;
    Ok(())
}

#[test]
fn exclusive_startup_invalidates_only_derived_task_metadata_then_named_prepare_rebuilds(
) -> Result<()> {
    let (dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (old, id) = seed(&db, &ledger)?;
    assert!(old.coverage().result_metadata[&id].activation_open);
    let aliases=db.transaction(|tx|Ok(tx.query_row("SELECT group_concat(family||record_id||content_digest||projection_digest,'|') FROM task_domain_records ORDER BY family,record_id",[],|r|r.get::<_,String>(0))?))?;
    let path = dir.path().join("startup-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&path)?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.try_lock().unwrap();
    recover(&db, &lease, &path, &guard, &ledger)?;
    db.transaction(|tx| {
        let coverage=current_coverage(tx,&scope())?.context("metadata missing")?;
        assert_eq!(coverage.revision,old.coverage().revision+1);
        assert!(!coverage.active_complete && !coverage.unsettled_complete && !coverage.stop.pending_complete);
        assert!(coverage.observations.is_empty());
        assert_eq!(tx.query_row("SELECT group_concat(family||record_id||content_digest||projection_digest,'|') FROM task_domain_records ORDER BY family,record_id",[],|r|r.get::<_,String>(0))?,aliases);
        assert!(load_task_domain_snapshot_tx(tx,&scope(),&request()).is_err(),"ordinary capture cannot bootstrap an invalidated view");
        let current=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&request(),&ledger)?.require_current()?.resident;
        assert_eq!(current.coverage().revision,coverage.revision);
        assert!(!current.coverage().result_metadata[&id].activation_open);
        Ok(())
    })?;
    assert!(old.coverage().result_metadata[&id].activation_open);
    let before = db
        .transaction(|tx| current_coverage(tx, &scope()))?
        .unwrap()
        .revision;
    recover(&db, &lease, &path, &guard, &ledger)?;
    assert_eq!(
        db.transaction(|tx| current_coverage(tx, &scope()))?
            .unwrap()
            .revision,
        before,
        "no actual Attempt change must not bump again"
    );
    drop(old);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn exclusive_startup_no_state_keeps_sparse_task_none_without_body_or_alias_grant() -> Result<()> {
    let (dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (old, _) = seed(&db, &ledger)?;
    let before = old.coverage().revision;
    drop(old);
    db.connection()?
        .execute("DELETE FROM agent_states WHERE agent_id='task-agent'", [])?;
    let path = dir.path().join("startup-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&path)?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.try_lock().unwrap();
    recover(&db, &lease, &path, &guard, &ledger)?;
    db.transaction(|tx| {
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM agent_states", [], |r| r
                .get::<_, usize>(0))?,
            0
        );
        let coverage = current_coverage(tx, &scope())?.unwrap();
        assert_eq!(coverage.revision, before + 1);
        assert!(!coverage.active_complete && !coverage.unsettled_complete);
        assert!(matches!(
            prepare_original_task_recovery_snapshot_with_budget_tx(
                tx,
                &scope(),
                &request(),
                &ledger
            )?,
            TaskBootstrapSnapshot::Retired { .. }
        ));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn exclusive_startup_task_metadata_fault_rolls_back_original_execution_and_queue() -> Result<()> {
    let (dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (old, _) = seed(&db, &ledger)?;
    db.connection()?.execute_batch("CREATE TRIGGER fail_startup_task_metadata BEFORE INSERT ON task_domain_current BEGIN SELECT RAISE(ABORT,'startup Task metadata fault'); END;")?;
    let path = dir.path().join("startup-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&path)?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.try_lock().unwrap();
    assert!(recover(&db, &lease, &path, &guard, &ledger).is_err());
    assert_eq!(db.connection()?.query_row("SELECT lifecycle_state FROM execution_protocol_attempts WHERE attempt_id='startup-task-attempt'",[],|r|r.get::<_,String>(0))?,"open");
    assert_eq!(
        db.connection()?.query_row(
            "SELECT status FROM queue_entries WHERE message_id='startup-task-source'",
            [],
            |r| r.get::<_, String>(0)
        )?,
        "dequeued"
    );
    assert_eq!(
        db.transaction(|tx| current_coverage(tx, &scope()))?
            .unwrap()
            .revision,
        old.coverage().revision
    );
    db.connection()?
        .execute_batch("DROP TRIGGER fail_startup_task_metadata")?;
    recover(&db, &lease, &path, &guard, &ledger)?;
    drop(old);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn exclusive_startup_task_tuple_budget_precedes_large_damaged_projection_decode() -> Result<()> {
    let (dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (old, _) = seed(&db, &ledger)?;
    drop(old);
    db.connection()?.execute("UPDATE execution_protocol_attempts SET payload_json=json_set(payload_json,'$.agent_id',hex(zeroblob(9*1024*1024))) WHERE attempt_id='startup-task-attempt'",[])?;
    let path = dir.path().join("startup-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&path)?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.try_lock().unwrap();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(&db, &lease, &path, &guard)?;
    let error = db
        .transaction(|tx| {
            prepare_exclusive_startup_task_metadata_tx(
                tx,
                &cap,
                &[StartupInputRecoveryScope::Current(scope())],
                &ledger,
            )
            .map(|_| ())
        })
        .err()
        .context("large tuple accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn exclusive_startup_current_legacy_identity_defaults_absent_incarnation_to_one() -> Result<()> {
    let (dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let (old, id) = seed(&db, &ledger)?;
    db.connection()?.execute("UPDATE agent_identities SET payload_json=json_remove(payload_json,'$.incarnation') WHERE agent_id='task-agent'", [])?;
    let path = dir.path().join("startup-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&path)?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.try_lock().unwrap();
    recover(&db, &lease, &path, &guard, &ledger)?;
    let current = db
        .transaction(|tx| {
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)
        })?
        .require_current()?
        .resident;
    assert!(!current.coverage().result_metadata[&id].activation_open);
    assert_eq!(current.scope(), &scope());
    drop(current);
    drop(old);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn exclusive_startup_identity_rejects_null_string_zero_and_foreign_incarnation() -> Result<()> {
    for value in ["null", "\"1\"", "0", "2"] {
        let (dir, db) = fresh()?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let (old, _) = seed(&db, &ledger)?;
        drop(old);
        db.connection()?.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',json(?1)) WHERE agent_id='task-agent'", [value])?;
        let path = dir.path().join("startup-maintenance.lock");
        let lease = RuntimeDbLock::try_lock(&path)?;
        let handoff = tokio::sync::Mutex::new(());
        let guard = handoff.try_lock().unwrap();
        let cap =
            ExclusiveStartupInputRecovery::for_pre_activation_host(&db, &lease, &path, &guard)?;
        let error = db
            .transaction(|tx| {
                prepare_exclusive_startup_task_metadata_tx(
                    tx,
                    &cap,
                    &[StartupInputRecoveryScope::Current(scope())],
                    &ledger,
                )
                .map(|_| ())
            })
            .err()
            .context("invalid incarnation accepted")?;
        assert!(
            format!("{error:#}").contains("identity/incarnation changed"),
            "{value}: {error:#}"
        );
        assert_eq!(db.connection()?.query_row("SELECT lifecycle_state FROM execution_protocol_attempts WHERE attempt_id='startup-task-attempt'", [], |r|r.get::<_,String>(0))?, "open");
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
