use super::super::loader::current_coverage;
use super::*;
use crate::runtime_db::{
    input_evidence_snapshot::legacy_recovery::{
        legacy_seed_recovery_scope_tx, ExclusiveStartupInputRecovery,
    },
    input_mutation_receipts::{
        insert_input_mutation_receipt_tx, lookup_input_mutation_receipt_tx, InputMutationOutcome,
        InputMutationResult, InputMutationScopeOutcome,
    },
    task_result_settlement::{prepare_upsert_pending_tx, TaskResultWriteLimits},
    transitions::{PreparedInputTransition, PreparedTaskTransition, TaskTransitionCommand},
    RuntimeDbLock,
};

fn cleanup_bundle(
    value: &TaskRecord,
    message: &MessageEnvelope,
    result: &TaskResultSettlementRecord,
    nonce: &str,
) -> Result<PreparedInputTransition> {
    PreparedInputTransition::task(
        scope(),
        nonce.into(),
        result.created_at,
        PreparedTaskTransition::new(
            TaskTransitionCommand {
                agent_id: scope().agent_id,
                expected_owner_incarnation: scope().identity_incarnation,
                task: value.clone(),
                agent_deletion: None,
                task_result_settlement: Some(result.clone()),
                queue_entry: None,
                work_items: vec![],
                expected_wait_conditions: vec![],
                wait_conditions: vec![],
                agent_state: None,
                message_evidence: vec![message.clone()],
                audit_events: vec![],
                index_changes: vec![],
                notify_scheduler: true,
                commit_on_idempotent: true,
                fault: None,
            },
            Default::default(),
            false,
            None,
            None,
        )?,
    )
}
fn admitted(id: &str) -> TaskRecord {
    let mut value = task(id, TaskStatus::Running);
    value.kind = TaskKind::AgentMessageWait;
    value.detail = Some(
        serde_json::json!({"rejoin_obligation_id":id,"rejoin_generation":1,"parent_turn_id":"original-parent-turn"}),
    );
    value
}
fn result_source(id: &str, task_id: &str) -> MessageEnvelope {
    let mut source = MessageEnvelope::new(
        "task-agent",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original cleanup result".into(),
        },
    );
    source.id = id.into();
    source.task_id = Some(task_id.into());
    source.created_at = at();
    source
}
fn original_result(
    value: &TaskRecord,
) -> Result<(TaskRecord, MessageEnvelope, TaskResultSettlementRecord)> {
    let mut final_task = value.clone();
    final_task.status = TaskStatus::Interrupted;
    final_task.updated_at = at() + Duration::seconds(1);
    let mut message = result_source(&format!("cleanup-result-{}", value.id), &value.id);
    message.created_at = final_task.updated_at;
    final_task.parent_message_id = Some(message.id.clone());
    let record =
        TaskResultSettlementRecord::pending(&final_task, &message, message.created_at)?.unwrap();
    Ok((final_task, message, record))
}
fn original_receipt(tx: &Transaction<'_>, bundle: &PreparedInputTransition) -> Result<()> {
    insert_input_mutation_receipt_tx(
        tx,
        bundle.descriptor(),
        &InputMutationOutcome {
            result: InputMutationResult::Task {
                task_id: bundle.original_task_cleanup()?.task().id.clone(),
                deletion: None,
                stop: None,
            },
            applied: true,
            delivery_receipt: None,
            scopes: vec![InputMutationScopeOutcome {
                scope: scope(),
                install_keys: Default::default(),
                audit_events: vec![],
                notify_memory_index: false,
                notify_scheduler: true,
            }],
        },
        at() + Duration::seconds(1),
    )?;
    Ok(())
}

#[test]
fn deleting_admitted_task_first_result_and_sparse_ack_share_original_transaction() -> Result<()> {
    let (_dir, db) = fresh()?;
    let before = admitted("cleanup-first");
    db.transaction(|tx| {
        write_task(tx, &before)?;
        let (identity, job, created) = repositories::ensure_agent_deletion_at_tx(
            tx,
            &repositories::AgentDeletionRequest {
                agent_id: scope().agent_id,
                admission: repositories::AgentDeletionAdmission::ExpectedRevision(
                    identity().revision,
                ),
                requested_by: "operator".into(),
                cascade_private_children: false,
            },
            "operator-cleanup-result",
            at() + Duration::seconds(1),
        )?;
        assert!(created);
        assert_eq!(identity.status, AgentRegistryStatus::Deleting);
        assert_eq!(job.mode, crate::types::AgentDeletionMode::Delete);
        tx.execute(
            "DELETE FROM agent_states WHERE agent_id=?1",
            [&scope().agent_id],
        )?;
        Ok(())
    })?;
    let (terminal, message, result) = original_result(&before)?;
    let bundle = cleanup_bundle(&terminal, &message, &result, "original-cleanup-first")?;
    db.transaction(|tx| {
        assert!(lookup_input_mutation_receipt_tx(tx, bundle.descriptor())?.is_none());
        let keys = BTreeSet::from([
            TaskKey::new(TaskFamily::Task, &before.id),
            TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
        ]);
        let pending =
            prepare_upsert_pending_tx(tx, &terminal, &result, TaskResultWriteLimits::default())?;
        assert!(original_task_result_cleanup_material_tx(tx, &scope(), &keys, &pending)?.bytes > 0);
        let existing = prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?;
        let proof = prepare_original_task_result_cleanup_tx(
            tx,
            existing,
            OriginalTaskResultCleanupSource::Task(bundle.original_task_cleanup()?),
            &pending,
        )?;
        let wrong_operation = crate::runtime_db::task_result_settlement::prepare_ensure_pending_tx(
            tx,
            &before.id,
            &message,
            message.created_at,
            Default::default(),
        )?;
        assert!(wrong_operation
            .execute_original_task_cleanup(&proof)
            .is_err());
        append_message_tx(tx, &message)?;
        repositories::upsert_task_tx(tx, &terminal)?;
        // Operator deletion preserves this original terminal producer. The cap
        // grants Result qualification, never an exception to the business fence.
        crate::runtime_db::reclamation::ensure_work_admission_tx(tx, &scope().agent_id)?;
        let ack = pending.execute_original_task_cleanup(&proof)?;
        assert_eq!(
            ack.actual_result_ids,
            BTreeSet::from([result.result_identity.clone()])
        );
        let sparse = register_original_task_result_cleanup_tx(tx, proof, &keys)?;
        assert_eq!(sparse.qualified_actual_keys, keys);
        assert!(sparse.invalidation.is_some());
        assert!(!current_coverage(tx, &scope())?.unwrap().unsettled_complete);
        assert!(alias(
            tx,
            &scope(),
            &TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity)
        )?
        .is_some_and(|a| a.availability == TaskAvailability::Present));
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM agent_states WHERE agent_id=?1",
                [&scope().agent_id],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        original_receipt(tx, &bundle)
    })?;
    assert!(db.transaction(|tx| Ok(
        lookup_input_mutation_receipt_tx(tx, bundle.descriptor())?.is_some()
    ))?);
    db.transaction(|tx| {
        let mut other = scope();
        other.identity_incarnation += 1;
        assert!(prepare_existing_task_maintenance_tx(
            tx,
            &other,
            &BTreeSet::from([TaskKey::new(TaskFamily::Task, &before.id)])
        )
        .is_err());
        assert!(matches!(
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?,
            TaskBootstrapSnapshot::Retired { .. }
        ));
        Ok(())
    })
}

#[test]
fn original_cleanup_complete_finalizer_updates_once_and_uses_actual_stored_result() -> Result<()> {
    let (_dir, db) = fresh()?;
    let before = admitted("cleanup-complete");
    db.transaction(|tx| {
        write_task(tx, &before)?;
        let mut id = identity();
        id.status = AgentRegistryStatus::Deleting;
        upsert_agent_identity_tx(tx, &id)
    })?;
    let (terminal, message, result) = original_result(&before)?;
    let bundle = cleanup_bundle(&terminal, &message, &result, "original-cleanup-complete")?;
    db.transaction(|tx| {
        let keys = BTreeSet::from([
            TaskKey::new(TaskFamily::Task, &before.id),
            TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
        ]);
        let old_revision = current_coverage(tx, &scope())?.unwrap().revision;
        let pending = prepare_upsert_pending_tx(tx, &terminal, &result, Default::default())?;
        let proof = prepare_original_task_result_cleanup_tx(
            tx,
            prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?,
            OriginalTaskResultCleanupSource::Task(bundle.original_task_cleanup()?),
            &pending,
        )?;
        append_message_tx(tx, &message)?;
        repositories::upsert_task_tx(tx, &terminal)?;
        let pending_ack = pending.execute_original_task_cleanup(&proof)?;
        let proof = proof.into_task_write_proof(tx, &pending_ack.actual_result_ids)?;
        let mut final_state = AgentState::new("task-agent");
        final_state.total_message_count = 1;
        upsert_agent_state_tx(tx, &final_state)?;
        let facts = register_task_writes_tx(tx, &proof, &keys, &final_state, &request())?;
        assert_eq!(facts.delta.revision, old_revision + 1);
        assert!(facts.delta.coverage.unsettled_complete);
        assert!(facts
            .delta
            .coverage
            .unsettled_result_ids
            .contains(&result.result_identity));
        let key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
        assert_eq!(
            facts
                .delta
                .records
                .iter()
                .find(|record| record.locator.key == key)
                .unwrap()
                .fact,
            CanonicalTaskFact::ResultSettlement(result.clone())
        );
        original_receipt(tx, &bundle)
    })
}

#[test]
fn derived_result_rejects_new_task_changed_generation_arbitrary_id_and_raw_collision() -> Result<()>
{
    for case in [
        "new-task",
        "changed-generation",
        "arbitrary-result",
        "raw-result",
    ] {
        let (_dir, db) = fresh()?;
        let before = admitted("cleanup-negative");
        if case != "new-task" {
            db.transaction(|tx| write_task(tx, &before).map(|_| ()))?;
        }
        let (mut terminal, message, mut result) = original_result(&before)?;
        if case == "changed-generation" {
            terminal.detail.as_mut().unwrap()["rejoin_generation"] = 2.into();
            result = TaskResultSettlementRecord::pending(&terminal, &message, message.created_at)?
                .unwrap();
        }
        if case == "arbitrary-result" {
            result.result_identity = "arbitrary-result-id".into();
        }
        if case == "raw-result" {
            db.transaction(|tx| upsert_pending_tx(tx, &result).map(|_| ()))?;
        }
        db.transaction(|tx| {
            let mut id = identity();
            id.status = AgentRegistryStatus::Deleting;
            upsert_agent_identity_tx(tx, &id)
        })?;
        let bundle = cleanup_bundle(&terminal, &message, &result, case)?;
        db.transaction(|tx| {
            let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &before.id)]);
            let pending = prepare_upsert_pending_tx(tx, &terminal, &result, Default::default())?;
            let existing = prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?;
            assert!(
                prepare_original_task_result_cleanup_tx(
                    tx,
                    existing,
                    OriginalTaskResultCleanupSource::Task(bundle.original_task_cleanup()?),
                    &pending
                )
                .is_err(),
                "{case}"
            );
            assert!(lookup_input_mutation_receipt_tx(tx, bundle.descriptor())?.is_none());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn derived_result_pruned_body_cannot_be_recreated_and_alias_fault_rolls_back_all() -> Result<()> {
    let (_dir, db) = fresh()?;
    let before = admitted("cleanup-rollback");
    db.transaction(|tx| {
        write_task(tx, &before)?;
        let mut id = identity();
        id.status = AgentRegistryStatus::Deleting;
        upsert_agent_identity_tx(tx, &id)
    })?;
    let (terminal, message, result) = original_result(&before)?;
    let bundle = cleanup_bundle(&terminal, &message, &result, "cleanup-rollback")?;
    db.connection()?.execute_batch("CREATE TRIGGER fail_cleanup_alias BEFORE INSERT ON task_domain_records WHEN NEW.family='result_settlement' BEGIN SELECT RAISE(ABORT,'cleanup alias failed'); END;")?;
    let error = db
        .transaction(|tx| {
            let keys = BTreeSet::from([
                TaskKey::new(TaskFamily::Task, &before.id),
                TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
            ]);
            let pending = prepare_upsert_pending_tx(tx, &terminal, &result, Default::default())?;
            let proof = prepare_original_task_result_cleanup_tx(
                tx,
                prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?,
                OriginalTaskResultCleanupSource::Task(bundle.original_task_cleanup()?),
                &pending,
            )?;
            append_message_tx(tx, &message)?;
            repositories::upsert_task_tx(tx, &terminal)?;
            pending.execute_original_task_cleanup(&proof)?;
            register_original_task_result_cleanup_tx(tx, proof, &keys)?;
            original_receipt(tx, &bundle)
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("cleanup alias failed"));
    db.transaction(|tx| {
        assert_eq!(
            stored_on(
                tx,
                &scope().agent_id,
                &TaskKey::new(TaskFamily::Task, &before.id)
            )?
            .unwrap()
            .fact,
            CanonicalTaskFact::Task(before.clone())
        );
        assert!(checkpoint_material_size_on(
            tx,
            &TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity)
        )?
        .is_none());
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM messages WHERE message_id=?1",
                [&message.id],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert!(lookup_input_mutation_receipt_tx(tx, bundle.descriptor())?.is_none());
        Ok(())
    })?;
    db.connection()?
        .execute_batch("DROP TRIGGER fail_cleanup_alias")?;
    db.transaction(|tx| {
        let task_key = TaskKey::new(TaskFamily::Task, &before.id);
        let result_key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
        let keys = BTreeSet::from([task_key.clone(), result_key.clone()]);
        let pending = prepare_upsert_pending_tx(tx, &terminal, &result, Default::default())?;
        let proof = prepare_original_task_result_cleanup_tx(
            tx,
            prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?,
            OriginalTaskResultCleanupSource::Task(bundle.original_task_cleanup()?),
            &pending,
        )?;
        repositories::upsert_task_tx(tx, &terminal)?;
        pending.execute_original_task_cleanup(&proof)?;
        register_original_task_result_cleanup_tx(tx, proof, &keys)?;
        let prune = prepare_existing_task_maintenance_tx(
            tx,
            &scope(),
            &BTreeSet::from([result_key.clone()]),
        )?;
        tx.execute(
            "DELETE FROM task_result_settlements WHERE result_identity=?1",
            [&result.result_identity],
        )?;
        register_existing_task_maintenance_tx(tx, prune, &BTreeSet::from([result_key.clone()]))?;
        assert!(alias(tx, &scope(), &result_key)?
            .is_some_and(|a| a.availability == TaskAvailability::Pruned));
        let pending = prepare_upsert_pending_tx(tx, &terminal, &result, Default::default())?;
        let existing =
            prepare_existing_task_maintenance_tx(tx, &scope(), &BTreeSet::from([task_key]))?;
        assert!(prepare_original_task_result_cleanup_tx(
            tx,
            existing,
            OriginalTaskResultCleanupSource::Task(bundle.original_task_cleanup()?),
            &pending
        )
        .is_err());
        Ok(())
    })
}

fn unbound_fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut connection, migration)?;
    }
    {
        let tx = connection.transaction()?;
        upsert_agent_state_tx(&tx, &AgentState::new("task-agent"))?;
        repositories::upsert_task_tx(&tx, &admitted("unbound-original"))?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[repositories::timestamp(at())])?;
        tx.commit()?;
    }
    drop(connection);
    let db = RuntimeDb::open_and_migrate(&path, dir.path().join("runtime.lock"))?;
    Ok((dir, db))
}
#[test]
fn exclusive_fixed_unbound_task_update_survives_original_binding_without_late_adoption(
) -> Result<()> {
    let (_dir, db) = unbound_fixture()?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let key = TaskKey::new(TaskFamily::Task, "unbound-original");
    let mut terminal = admitted("unbound-original");
    terminal.status = TaskStatus::Interrupted;
    terminal.updated_at = at() + Duration::seconds(1);
    let seed=db.transaction(|tx|{
        let seed=legacy_seed_recovery_scope_tx(tx,"task-agent")?;
        let original=qualification::eligible_digest(tx,Some(&seed.seed_id),&key)?.unwrap();
        let keys=BTreeSet::from([key.clone()]);assert!(unbound_task_maintenance_material_tx(tx,&seed,&keys)?.bytes>0);
        let proof=prepare_unbound_task_maintenance_tx(tx,&cap,&seed,&keys)?;
        repositories::upsert_task_tx(tx,&terminal)?;
        let ack=register_unbound_task_maintenance_tx(tx,proof,&keys)?;assert_eq!(ack.actual_fixed_keys,keys);
        assert_eq!(ack.seed,seed);
        let immutable:(String,String)=tx.query_row("SELECT content_digest,projection_digest FROM task_domain_eligibility WHERE seed_id=?1 AND family='task' AND record_id=?2 LIMIT 1",params![seed.seed_id,key.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(immutable,original);
        repositories::upsert_task_tx(tx,&admitted("late-unqualified"))?;
        assert!(prepare_unbound_task_maintenance_tx(tx,&cap,&seed,&BTreeSet::from([TaskKey::new(TaskFamily::Task,"late-unqualified")])).is_err());
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM agent_identities",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_domain_current",[],|r|r.get::<_,i64>(0))?,0);
        Ok(seed)
    })?;
    db.transaction(|tx| {
        assert!(
            crate::runtime_db::episode_snapshot::bind_unbound_episode_continuity_tx(
                tx,
                &identity(),
                at() + Duration::seconds(2)
            )?
        );
        upsert_agent_identity_tx(tx, &identity())?;
        let snapshot =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert_eq!(
            snapshot
                .resident
                .capture(&request().capture)?
                .lookup(&key)?
                .unwrap()
                .fact,
            CanonicalTaskFact::Task(terminal.clone())
        );
        assert!(prepare_unbound_task_maintenance_tx(
            tx,
            &cap,
            &seed,
            &BTreeSet::from([key.clone()])
        )
        .is_err());
        let late = TaskKey::new(TaskFamily::Task, "late-unqualified");
        assert!(alias(tx, &scope(), &late)?.is_none());
        let mut wrong = scope();
        wrong.identity_incarnation += 1;
        assert!(matches!(
            prepare_task_domain_snapshot_tx(tx, &wrong, &request())?,
            TaskBootstrapSnapshot::Retired { .. }
        ));
        Ok(())
    })
}
#[test]
fn exclusive_unbound_wrong_database_and_overlay_fault_never_leave_business_half_commit(
) -> Result<()> {
    let (_dir, db) = unbound_fixture()?;
    let (_other_dir, other) = unbound_fixture()?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    other.transaction(|tx| {
        assert!(cap.validate_transaction(tx).is_err());
        Ok(())
    })?;
    db.connection()?.execute_batch("CREATE TRIGGER fail_seed_overlay BEFORE INSERT ON task_domain_seed_recovery_current BEGIN SELECT RAISE(ABORT,'seed overlay failed'); END;")?;
    let key = TaskKey::new(TaskFamily::Task, "unbound-original");
    let error = db
        .transaction(|tx| {
            let seed = legacy_seed_recovery_scope_tx(tx, "task-agent")?;
            let keys = BTreeSet::from([key.clone()]);
            let proof = prepare_unbound_task_maintenance_tx(tx, &cap, &seed, &keys)?;
            let mut terminal = admitted("unbound-original");
            terminal.status = TaskStatus::Interrupted;
            terminal.updated_at = at() + Duration::seconds(1);
            repositories::upsert_task_tx(tx, &terminal)?;
            register_unbound_task_maintenance_tx(tx, proof, &keys).map(|_| ())
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("seed overlay failed"));
    db.transaction(|tx| {
        assert_eq!(
            stored_on(tx, "task-agent", &key)?.unwrap().fact,
            CanonicalTaskFact::Task(admitted("unbound-original"))
        );
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM task_domain_seed_recovery_current",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    })
}

#[test]
fn control_cleanup_view_uses_real_stop_snapshot_and_closed_original_result() -> Result<()> {
    use crate::{
        runtime_db::control_transition::{
            prepare_control, ControlCancellationKind, ControlCancellationTarget, ControlInvocation,
            ControlKey, ControlPlanInput, ControlSource, ControlTaskResult,
        },
        types::ControlAction,
    };
    let (_dir, db) = fresh()?;
    let before = admitted("control-original");
    db.transaction(|tx| write_task(tx, &before).map(|_| ()))?;
    let key = ControlKey {
        agent_id: scope().agent_id,
        identity_incarnation: 1,
        command_id: "original-control-cleanup".into(),
    };
    let snapshot = db.load_control_snapshot(&key)?;
    let (mut terminal, mut message, _) = original_result(&before)?;
    terminal.detail.as_mut().unwrap()["interrupted_reason"] = "agent_stopped".into();
    terminal.detail.as_mut().unwrap()["status_before_stop"] = "running".into();
    message.turn_id = Some("original-parent-turn".into());
    message.metadata = Some(
        serde_json::json!({"task_detail":terminal.detail,"interrupted_reason":"agent_stopped"}),
    );
    let result =
        TaskResultSettlementRecord::pending(&terminal, &message, message.created_at)?.unwrap();
    let command = prepare_control(ControlPlanInput {
        key,
        invocation: ControlInvocation {
            action: ControlAction::Stop,
            source: ControlSource::DirectApi,
            authority_class: AuthorityClass::OperatorInstruction,
        },
        snapshot,
        prepared_at: message.created_at,
        task_results: vec![ControlTaskResult {
            task: terminal.clone(),
            message: message.clone(),
        }],
        healthy_running: None,
        protected_control_attempt: None,
        protected_control_run_id: None,
        cancellation_targets: vec![ControlCancellationTarget {
            obligation_id: "original-control-task-obligation".into(),
            target_kind: ControlCancellationKind::Task,
            target_identity: before.id.clone(),
            target_generation: serde_json::json!({"owner_epoch":1,"created_at":before.created_at,
                "rejoin":before.rejoin_fence().ok(),"runtime_lease_id":null}),
            target: serde_json::json!({"kind":before.kind,"recovery":before.recovery,"work_item_id":before.effective_work_item_id()}),
        }],
        audit_events: vec![],
        index_changes: vec![],
    })?;
    assert!(command.original_task_cleanup("different-task").is_err());
    db.transaction(|tx| {
        let keys = BTreeSet::from([
            TaskKey::new(TaskFamily::Task, &before.id),
            TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity),
        ]);
        let pending = prepare_upsert_pending_tx(tx, &terminal, &result, Default::default())?;
        let proof = prepare_original_task_result_cleanup_tx(
            tx,
            prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?,
            OriginalTaskResultCleanupSource::Control(command.original_task_cleanup(&before.id)?),
            &pending,
        )?;
        repositories::upsert_task_tx(tx, &terminal)?;
        pending.execute_original_task_cleanup(&proof)?;
        let ack = register_original_task_result_cleanup_tx(tx, proof, &keys)?;
        assert_eq!(ack.qualified_actual_keys, keys);
        Ok(())
    })
}

#[test]
fn original_task_cleanup_cannot_bypass_parent_cleanup_admission_fence() -> Result<()> {
    let (_dir, db) = fresh()?;
    // A terminal Task can precede ParentCleanup admission. A delayed Result
    // producer must still respect the durable fence when no Result existed yet.
    let (terminal, message, result) = original_result(&admitted("fenced-late-result"))?;
    db.transaction(|tx| {
        write_task(tx, &terminal)?;
        let mut id = identity();
        let revision = id.revision;
        id.status = AgentRegistryStatus::Deleting;
        id.revision += 1;
        id.updated_at += Duration::nanoseconds(1);
        upsert_agent_identity_tx(tx, &id)?;
        crate::runtime_db::evidence::insert_agent_deletion_job_tx(
            tx,
            &crate::types::AgentDeletionJob {
                archive_path: None,
                deletion_id: "parent-fenced-late-result".into(),
                agent_id: scope().agent_id,
                mode: crate::types::AgentDeletionMode::ParentCleanup,
                status: crate::types::AgentDeletionStatus::Pending,
                phase: crate::types::AgentDeletionPhase::Fence,
                requested_by: "original-parent".into(),
                expected_identity_revision: revision,
                cascade_private_children: false,
                attempts: 0,
                last_error: None,
                next_attempt_at: None,
                created_at: id.updated_at,
                updated_at: id.updated_at,
                completed_at: None,
            },
        )?;
        Ok(())
    })?;
    let bundle = cleanup_bundle(&terminal, &message, &result, "parent-fenced-late-result")?;
    let task_key = TaskKey::new(TaskFamily::Task, &terminal.id);
    let result_key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
    let before = db.transaction(|tx| Ok((
        current_coverage(tx, &scope())?,
        tx.query_row("SELECT payload_json FROM agent_states WHERE agent_id=?1", [&scope().agent_id], |r| r.get::<_,String>(0))?,
        tx.query_row("SELECT json_array(revision,availability,content_digest,projection_digest) FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3",params![scope().agent_id,scope().identity_incarnation,terminal.id],|r|r.get::<_,String>(0))?,
        tx.query_row("SELECT COUNT(*) FROM audit_events", [], |r|r.get::<_,i64>(0))?,
    )))?;
    let error = db
        .transaction(|tx| {
            let keys = BTreeSet::from([task_key.clone(), result_key.clone()]);
            let pending = prepare_upsert_pending_tx(tx, &terminal, &result, Default::default())?;
            let proof = prepare_original_task_result_cleanup_tx(
                tx,
                prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?,
                OriginalTaskResultCleanupSource::Task(bundle.original_task_cleanup()?),
                &pending,
            )?;
            // This is the real producer order before pending insertion. Both the
            // Message append and any later finalizer/receipt must roll back on fence.
            append_message_tx(tx, &message)?;
            assert!(!repositories::upsert_task_tx(tx, &terminal)?);
            pending.execute_original_task_cleanup(&proof)?;
            register_original_task_result_cleanup_tx(tx, proof, &keys)?;
            original_receipt(tx, &bundle)
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("agent_cleanup_fenced"));
    db.transaction(|tx| {
        assert_eq!(stored_on(tx,&scope().agent_id,&task_key)?.unwrap().fact,
            CanonicalTaskFact::Task(terminal.clone()));
        assert!(checkpoint_material_size_on(tx,&result_key)?.is_none());
        assert!(alias(tx,&scope(),&result_key)?.is_none());
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM messages WHERE message_id=?1",[&message.id],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM queue_entries WHERE message_id=?1",[&message.id],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(current_coverage(tx,&scope())?,before.0);
        assert_eq!(tx.query_row("SELECT payload_json FROM agent_states WHERE agent_id=?1",[&scope().agent_id],|r|r.get::<_,String>(0))?,before.1);
        assert_eq!(tx.query_row("SELECT json_array(revision,availability,content_digest,projection_digest) FROM task_domain_records WHERE agent_id=?1 AND identity_incarnation=?2 AND family='task' AND record_id=?3",params![scope().agent_id,scope().identity_incarnation,terminal.id],|r|r.get::<_,String>(0))?,before.2);
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM audit_events",[],|r|r.get::<_,i64>(0))?,before.3);
        assert!(lookup_input_mutation_receipt_tx(tx,bundle.descriptor())?.is_none());
        Ok(())
    })
}

#[test]
fn fixed_task_digest_length_gate_precedes_string_decode_and_releases_candidate_guard() -> Result<()>
{
    use crate::runtime::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
    for column in ["content_digest", "projection_digest"] {
        let value = admitted("bounded-fixed-digest");
        let (_dir, db) = legacy(true, |tx| {
            repositories::upsert_task_tx(tx, &value).map(|_| ())
        })?;
        let key = TaskKey::new(TaskFamily::Task, &value.id);
        let seed =
            db.transaction(|tx| binding(tx, &scope())?.context("fixed Task seed missing"))?;
        db.transaction(|tx| {
            // Explicit corruption: SQLite holds the large TEXT. Rust must reject
            // its integer length before allocating it, or decoding the bad body.
            let sql=match column {
                "content_digest"=>"UPDATE task_domain_eligibility SET content_digest=CAST(zeroblob(8388608) AS TEXT) WHERE seed_id=?1 AND family='task' AND record_id=?2",
                _=>"UPDATE task_domain_eligibility SET projection_digest=CAST(zeroblob(8388608) AS TEXT) WHERE seed_id=?1 AND family='task' AND record_id=?2",
            };
            assert!(tx.execute(sql,params![seed,key.id])?>0);
            tx.execute("UPDATE tasks SET payload_json='{' WHERE task_id=?1",[&key.id])?;
            Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let error = db
            .transaction(|tx| {
                let material =
                    task_maintenance_material_tx(tx, &scope(), &BTreeSet::from([key.clone()]))?;
                assert!(material.bytes < 1024 * 1024);
                let _guard = ledger.register_owned_bytes(
                    &scope(),
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate {
                        payload_bytes: material.bytes as u64,
                        metadata_estimate_bytes: 0,
                    },
                )?;
                assert!(ledger.report()?.observed_bytes > 0);
                qualification::eligible_digest(tx, Some(&seed), &key).map(|_| ())
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("fixed Task eligibility digest length changed"));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        db.transaction(|tx| {
            assert!(alias(tx, &scope(), &key)?.is_none());
            assert!(current_coverage(tx, &scope())?.is_none());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn unbound_finalizer_checks_fixed_digest_before_decode_and_rolls_back_actual_task() -> Result<()> {
    let (_dir, db) = unbound_fixture()?;
    let lease = RuntimeDbLock::lock(db.lock_path())?;
    let handoff = tokio::sync::Mutex::new(());
    let guard = handoff.blocking_lock();
    let cap = ExclusiveStartupInputRecovery::for_pre_activation_host(
        &db,
        &lease,
        db.lock_path(),
        &guard,
    )?;
    let key = TaskKey::new(TaskFamily::Task, "unbound-original");
    let keys = BTreeSet::from([key.clone()]);
    let mut terminal = admitted(&key.id);
    terminal.status = TaskStatus::Interrupted;
    terminal.updated_at = at() + Duration::seconds(1);
    let error=db.transaction(|tx| {
        let seed=legacy_seed_recovery_scope_tx(tx,"task-agent")?;
        let proof=prepare_unbound_task_maintenance_tx(tx,&cap,&seed,&keys)?;
        repositories::upsert_task_tx(tx,&terminal)?;
        // Damage only the fixed-origin column after preproof, exercising the
        // finalizer's direct fixed read, rather than just its prepare fallback.
        tx.execute("UPDATE task_domain_eligibility SET projection_digest=CAST(zeroblob(8388608) AS TEXT) WHERE seed_id=?1 AND family='task' AND record_id=?2",params![seed.seed_id,key.id])?;
        register_unbound_task_maintenance_tx(tx,proof,&keys).map(|_|())
    }).unwrap_err();
    assert!(format!("{error:#}").contains("fixed Task eligibility digest length changed"));
    db.transaction(|tx| {
        assert_eq!(stored_on(tx,"task-agent",&key)?.unwrap().fact,CanonicalTaskFact::Task(admitted(&key.id)));
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_domain_seed_recovery_current",[],|r|r.get::<_,i64>(0))?,0);
        let seed=legacy_seed_recovery_scope_tx(tx,"task-agent")?;
        assert!(qualification::fixed_eligible_digest(tx,&seed.seed_id,&key)?.is_some());
        tx.execute("UPDATE task_domain_eligibility SET content_digest=?3 WHERE seed_id=?1 AND family='task' AND record_id=?2",params![seed.seed_id,key.id,"z".repeat(64)])?;
        let error=qualified_unbound_task_maintenance_keys_tx(tx,&cap,&seed,&keys).unwrap_err();
        assert!(format!("{error:#}").contains("invalid fixed Task eligibility digest"));
        tx.execute("UPDATE task_domain_captures SET metadata_digest=CAST(zeroblob(8388608) AS TEXT) WHERE seed_id=?1",[&seed.seed_id])?;
        let error=prepare_unbound_task_maintenance_tx(tx,&cap,&seed,&keys).err()
            .context("damaged checkpoint digest accepted")?;
        assert!(format!("{error:#}").contains("fixed Task checkpoint digest length changed"));
        Ok(())
    })
}
