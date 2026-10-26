use super::loader::current_coverage;
use super::*;
use crate::{
    config::StateMemoryLimits,
    domain::execution_protocol::{
        admit_execution, AdmitExecution, AdmittedFences, ExecutionAttempt, ExecutionAttemptState,
        ExecutionBinding, ExecutionOrigin, ExecutionPriority, ExecutionProtocolState,
        ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
    },
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        task_result_settlement::{
            upsert_pending_tx, TaskResultSettlementRecord, TaskResultSettlementState,
        },
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        TaskKind, TaskRecord, TaskStatus,
    },
};
use chrono::{DateTime, Duration, Utc};

pub(super) fn at() -> DateTime<Utc> {
    "2026-10-01T00:00:00.123456789Z".parse().unwrap()
}
pub(super) fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "task-agent".into(),
        identity_incarnation: 1,
    }
}
pub(super) fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        "task-agent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}
pub(super) fn request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
pub(super) fn task(id: &str, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        agent_id: "task-agent".into(),
        kind: TaskKind::CommandTask,
        status,
        created_at: at(),
        updated_at: at(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some(id.into()),
        detail: None,
        recovery: None,
    }
}
pub(super) fn fresh() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        upsert_agent_identity_tx(tx, &identity())?;
        upsert_agent_state_tx(tx, &AgentState::new("task-agent"))?;
        prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        Ok(())
    })?;
    Ok((dir, db))
}
pub(super) fn legacy(
    sealed: bool,
    prepare: impl FnOnce(&Transaction<'_>) -> Result<()>,
) -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite");
    let mut conn = Connection::open(&path)?;
    ensure_migration_table(&conn)?;
    for m in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut conn, m)?;
    }
    {
        let tx = conn.transaction()?;
        upsert_agent_identity_tx(&tx, &identity())?;
        upsert_agent_state_tx(&tx, &AgentState::new("task-agent"))?;
        if sealed {
            tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[repositories::timestamp(at())])?;
        }
        prepare(&tx)?;
        tx.commit()?;
    }
    drop(conn);
    let db = RuntimeDb::open_and_migrate(&path, dir.path().join("runtime.lock"))?;
    Ok((dir, db))
}
fn write_task(tx: &Transaction<'_>, value: &TaskRecord) -> Result<CommittedTaskFacts> {
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &value.id)]);
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    repositories::upsert_task_tx(tx, value)?;
    register_task_writes_tx(
        tx,
        &proof,
        &keys,
        &AgentState::new("task-agent"),
        &request(),
    )
}

#[test]
fn actual_slim_ack_noop_and_selected_arc_survive_without_domain_pin() -> Result<()> {
    let (_dir, db) = fresh()?;
    let before = db
        .transaction(|tx| {
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    let mut value = task("terminal-slim", TaskStatus::Completed);
    value.detail =
        Some(serde_json::json!({"initial_output":"x".repeat(10000),"kept":"y".repeat(3000)}));
    let facts = db.transaction(|tx| write_task(tx, &value))?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let resident = before
        .with_committed(&facts.delta)?
        .with_budget(ledger.clone())?;
    let capture = resident.capture(&request().capture)?;
    let qualified = capture
        .lookup(&TaskKey::new(TaskFamily::Task, &value.id))?
        .unwrap();
    assert_eq!(
        qualified.fact,
        CanonicalTaskFact::Task(repositories::slim_task_record_for_payload(&value))
    );
    let weak = Arc::downgrade(qualified);
    let noop = db.transaction(|tx| {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &value.id)]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &request(),
        )
    })?;
    assert_eq!(noop.delta.base_revision, noop.delta.revision);
    assert!(noop.delta.records.is_empty());
    assert_eq!(
        resident.with_committed(&noop.delta)?.coverage(),
        resident.coverage()
    );
    drop(resident);
    drop(facts);
    drop(before);
    assert!(weak.upgrade().is_some());
    assert_eq!(ledger.report()?.physical_records.allocations, 1);
    drop(capture);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.physical_records.allocations, 0);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn complete_unsettled_frontier_includes_caller_admitted_open_activation() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx|{
        let mut source=MessageEnvelope::new("task-agent",MessageKind::TaskResult,
            MessageOrigin::Task{task_id:"completed-task".into()},AuthorityClass::RuntimeInstruction,
            Priority::Normal,MessageBody::Text{text:"completed".into()});
        source.id="result-message".into();source.task_id=Some("completed-task".into());source.created_at=at();
        let (source,_)=append_message_tx(tx,&source)?;
        let mut value=task("completed-task",TaskStatus::Completed);
        value.parent_message_id=Some(source.id.clone());
        value.detail=Some(serde_json::json!({"rejoin_obligation_id":value.id,"rejoin_generation":1,"parent_turn_id":"original-parent-turn"}));
        let mut result=TaskResultSettlementRecord::pending(&value,&source,at())?.unwrap();
        result.state=TaskResultSettlementState::CallerAdmitted;
        result.activation_id=Some("original-open-activation".into());result.admitted_at=Some(at());
        let keys=BTreeSet::from([TaskKey::new(TaskFamily::Task,&value.id),TaskKey::new(TaskFamily::ResultSettlement,&result.result_identity)]);
        let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
        repositories::upsert_task_tx(tx,&value)?;upsert_pending_tx(tx,&result)?;
        let sequence=source.message_seq.context("actual source sequence")?;
        let admitted=admit_execution(&ExecutionProtocolState::empty("task-agent"),&AdmitExecution{attempt:ExecutionAttempt{
            attempt_id:"original-open-activation".into(),agent_id:"task-agent".into(),source_message_id:Some(source.id.clone()),
            source:ExecutionSource{identity:ExecutionSourceIdentity::QueueMessage{message_id:source.id.clone()},generation:sequence},
            binding:ExecutionBinding::Conversation{interaction_id:source.id.clone()},
            provenance:ExecutionProvenance{origin:ExecutionOrigin::Task,trust:ExecutionTrust::RuntimeInstruction,priority:ExecutionPriority::Normal,correlation_id:None,causation_id:None},
            admitted_fences:AdmittedFences{source_revision:sequence,work_item_source_revision:None,work_item_generation:None,rejoin:None,agent_control_revision:1,host_registry_revision:1},
            state:ExecutionAttemptState::Open,run_id:None,turn_id:None,recovery_of_attempt_id:None,terminal_outcome_id:None,admitted_at:at().to_rfc3339(),terminal_at:None,
        }}).map_err(anyhow::Error::msg)?;
        crate::runtime_db::transitions::persist_state_tx(tx,&admitted.state)?;
        let mut state=AgentState::new("task-agent");state.total_message_count=1;upsert_agent_state_tx(tx,&state)?;
        let facts=register_task_writes_tx(tx,&proof,&keys,&state,&request())?;
        assert!(facts.delta.coverage.unsettled_complete);
        assert!(facts.delta.coverage.unsettled_result_ids.contains(&result.result_identity));
        assert!(facts.delta.coverage.result_metadata[&result.result_identity].activation_open);
        let loaded=load_task_domain_snapshot_tx(tx,&scope(),&request())?.require_current()?;
        let capture=loaded.resident.capture(&request().capture)?;
        assert_eq!(capture.unsettled_for_owner(None,true)?.len(),1);
        assert!(capture.unsettled_for_owner(None,false)?.is_empty());
        assert_eq!(capture.next_recheck_at()?,None);
        Ok(())
    })
}

#[test]
fn original_v81_cutover_freezes_terminal128_and_never_adopts_later_or_new_incarnation() -> Result<()>
{
    let (_dir, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &task("fixed-active", TaskStatus::Running))?;
        for i in 0..130 {
            repositories::upsert_task_tx(
                tx,
                &task(&format!("terminal-{i:03}"), TaskStatus::Completed),
            )?;
        }
        Ok(())
    })?;
    db.transaction(|tx| {
        repositories::upsert_task_tx(tx, &task("late-unqualified", TaskStatus::Completed))?;
        let loaded =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert_eq!(loaded.resident.coverage().terminal.keys.len(), 128);
        assert!(!loaded.resident.coverage().terminal.is_exhaustive);
        assert!(loaded
            .resident
            .coverage()
            .active_task_ids
            .contains("fixed-active"));
        let mut explicit = request();
        explicit
            .capture
            .required_keys
            .insert(TaskKey::new(TaskFamily::Task, "late-unqualified"));
        assert!(prepare_task_domain_snapshot_tx(tx, &scope(), &explicit).is_err());
        let mut deleting = identity();
        deleting.status = AgentRegistryStatus::Deleting;
        upsert_agent_identity_tx(tx, &deleting)?;
        assert!(matches!(
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?,
            TaskBootstrapSnapshot::Retired { .. }
        ));
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "fixed-active")]);
        assert!(prepare_task_write_tx(tx, &scope(), &keys).is_err());
        let proof = prepare_original_task_cleanup_tx(tx, &scope(), &keys)?;
        preflight_task_write_snapshot_tx(tx, &proof, &request())?.require_current()?;
        let mut successor = identity();
        successor.incarnation = 2;
        upsert_agent_identity_tx(tx, &successor)?;
        let next = InputEvidenceScope {
            agent_id: scope().agent_id,
            identity_incarnation: 2,
        };
        assert!(matches!(
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?,
            TaskBootstrapSnapshot::Retired { .. }
        ));
        let current = prepare_task_domain_snapshot_tx(tx, &next, &request())?.require_current()?;
        assert!(current.resident.coverage().active_task_ids.is_empty());
        assert!(current.resident.coverage().terminal.keys.is_empty());
        assert!(prepare_task_write_tx(tx, &next, &keys).is_err());
        Ok(())
    })
}

#[test]
fn final_alias_failure_rolls_back_original_task_and_metadata() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx|{
        tx.execute_batch("CREATE TRIGGER fail_task_alias BEFORE INSERT ON task_domain_records BEGIN SELECT RAISE(ABORT,'task alias fault'); END;")?;
        Ok(())
    })?;
    let before = db.transaction(|tx| Ok(current_coverage(tx, &scope())?))?;
    let error = db
        .transaction(|tx| write_task(tx, &task("must-rollback", TaskStatus::Running)))
        .unwrap_err();
    assert!(format!("{error:#}").contains("task alias fault"));
    db.transaction(|tx| {
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM tasks WHERE task_id='must-rollback'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(current_coverage(tx, &scope())?, before);
        Ok(())
    })
}

#[test]
fn explicit_absence_is_not_unknown_and_pruned_id_cannot_be_revived() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| write_task(tx, &task("prunable", TaskStatus::Completed)))?;
    let before = db
        .transaction(|tx| {
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    let removed = db.transaction(|tx| {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "prunable")]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        tx.execute("DELETE FROM tasks WHERE task_id='prunable'", [])?;
        let mut explicit = request();
        explicit
            .capture
            .required_keys
            .insert(TaskKey::new(TaskFamily::Task, "prunable"));
        register_task_writes_tx(tx, &proof, &keys, &AgentState::new("task-agent"), &explicit)
    })?;
    let mut policy = request().capture;
    policy
        .required_keys
        .insert(TaskKey::new(TaskFamily::Task, "prunable"));
    let after = before.with_committed(&removed.delta)?;
    assert!(after
        .capture(&policy)?
        .lookup(&TaskKey::new(TaskFamily::Task, "prunable"))?
        .is_none());
    db.transaction(|tx| {
        assert!(prepare_task_write_tx(tx, &scope(), &policy.required_keys).is_err());
        let mut explicit = request();
        explicit
            .capture
            .required_keys
            .insert(TaskKey::new(TaskFamily::Task, "never-existed"));
        let captured = prepare_task_domain_snapshot_tx(tx, &scope(), &explicit)?
            .require_current()?
            .resident
            .capture(&explicit.capture)?;
        assert!(captured
            .lookup(&TaskKey::new(TaskFamily::Task, "never-existed"))?
            .is_none());
        assert!(captured
            .lookup(&TaskKey::new(TaskFamily::Task, "unobserved"))
            .is_err());
        Ok(())
    })
}

#[test]
fn same_millis_canonical_change_advances_locator_and_raw_backwrite_is_rejected() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut value = task("same-millis", TaskStatus::Running);
    let first = db.transaction(|tx| write_task(tx, &value))?;
    let old = first
        .delta
        .records
        .iter()
        .find(|x| x.locator.key.id == value.id)
        .unwrap()
        .locator
        .clone();
    value.summary = Some("changed without changing timestamp".into());
    let next = db.transaction(|tx| write_task(tx, &value))?;
    let actual = next
        .delta
        .records
        .iter()
        .find(|x| x.locator.key.id == value.id)
        .unwrap();
    assert!(actual.locator.revision > old.revision);
    assert_ne!(actual.locator.content_digest, old.content_digest);
    assert_eq!(value.updated_at, at());
    db.transaction(|tx| {
        // A restore outside the owner/writer protocol must be detected, not adopted.
        tx.execute("UPDATE tasks SET summary='foreign restore',payload_json=json_set(payload_json,'$.summary','foreign restore') WHERE task_id=?1",[&value.id])?;
        assert!(prepare_task_domain_snapshot_tx(tx,&scope(),&request()).is_err());
        Ok(())
    })
}

#[test]
fn final_combined_body_metadata_budget_failure_keeps_canonical_and_coverage_unchanged() -> Result<()>
{
    let (_dir, db) = fresh()?;
    let before = db.transaction(|tx| current_coverage(tx, &scope()))?;
    let value = task("budget-rollback", TaskStatus::Running);
    let error = db
        .transaction(|tx| {
            let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &value.id)]);
            let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
            repositories::upsert_task_tx(tx, &value)?;
            let mut tiny = request();
            tiny.byte_limit = 64;
            register_task_writes_tx(tx, &proof, &keys, &AgentState::new("task-agent"), &tiny)
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("ResourceExhausted"));
    db.transaction(|tx| {
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM tasks WHERE task_id=?1",
                [&value.id],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM task_domain_records WHERE record_id=?1",
                [&value.id],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(current_coverage(tx, &scope())?, before);
        Ok(())
    })
}

#[test]
fn finalizer_installs_complete_active_body_despite_narrow_prompt_capture() -> Result<()> {
    let (_dir, db) = fresh()?;
    let before = db
        .transaction(|tx| {
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    let value = task("new-running", TaskStatus::Running);
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &value.id)]);
    let mut narrow = request();
    narrow.capture.include_active = false;
    narrow.capture.include_unsettled = false;
    narrow.capture.terminal_limit = 0;
    let facts = db.transaction(|tx| {
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &value)?;
        register_task_writes_tx(tx, &proof, &keys, &AgentState::new("task-agent"), &narrow)
    })?;
    assert!(facts.delta.coverage.active_task_ids.contains(&value.id));
    assert!(facts
        .delta
        .records
        .iter()
        .any(|x| x.locator.key == TaskKey::new(TaskFamily::Task, &value.id)));
    let installed = before.with_committed(&facts.delta)?;
    assert!(installed.capture(&narrow.capture)?.records.is_empty());
    let current = installed.capture(&TaskCaptureRequest::current())?;
    assert!(current
        .records
        .iter()
        .any(|x| x.locator.key == TaskKey::new(TaskFamily::Task, &value.id)));
    Ok(())
}

#[test]
fn full_task_projection_rejects_sql_only_changes_but_keeps_original_slim_semantics() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut value = task("long-normalized-detail", TaskStatus::Completed);
    value.detail = Some(serde_json::json!({
        "child_agent_id":"original-child",
        "input_target":"i".repeat(9000),
        "output_path":"p".repeat(9000),
        "output_summary":"s".repeat(9000),
        "exit_status":23,
        "terminal_reentry":true,
    }));
    db.transaction(|tx| write_task(tx, &value))?;
    db.transaction(|tx| {
        let key = TaskKey::new(TaskFamily::Task, &value.id);
        let stored = stored_on(tx, "task-agent", &key)?.unwrap();
        assert_eq!(stored.fact, CanonicalTaskFact::Task(repositories::slim_task_record_for_payload(&value)));
        assert!(validate_task_domain_observation_tx(tx, &scope(), current_coverage(tx, &scope())?.unwrap().revision).is_ok());
        tx.execute("UPDATE tasks SET child_agent_id='foreign-child' WHERE task_id=?1",[&value.id])?;
        assert!(stored_on(tx, "task-agent", &key).is_err());
        tx.execute("UPDATE tasks SET child_agent_id='original-child',last_turn_id='sql-only-corruption' WHERE task_id=?1",[&value.id])?;
        let actual = stored_on(tx, "task-agent", &key)?.unwrap();
        assert!(qualification::validate_member(tx, &scope(), None, &key, &actual).is_err());
        Ok(())
    })
}

#[test]
fn fixed_unmaterialized_task_projection_is_frozen_at_original_upgrade() -> Result<()> {
    let (_dir, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &task("fixed-task", TaskStatus::Completed))?;
        Ok(())
    })?;
    db.transaction(|tx| {
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM task_domain_records WHERE record_id='fixed-task'",
                [],
                |r| r.get::<_, usize>(0)
            )?,
            0
        );
        // Simulate a conflicting restore after fixed capture, before first preparation.
        tx.execute(
            "UPDATE tasks SET correlation_id='late-restore' WHERE task_id='fixed-task'",
            [],
        )?;
        let key = TaskKey::new(TaskFamily::Task, "fixed-task");
        let actual = stored_on(tx, "task-agent", &key)?.unwrap();
        assert!(qualification::validate_member(
            tx,
            &scope(),
            binding(tx, &scope())?.as_deref(),
            &key,
            &actual
        )
        .is_err());
        assert!(prepare_task_domain_snapshot_tx(tx, &scope(), &request()).is_err());
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM task_domain_records WHERE record_id='fixed-task'",
                [],
                |r| r.get::<_, usize>(0)
            )?,
            0
        );
        Ok(())
    })
}

#[path = "read_preparation_tests.rs"]
mod read_preparation;

#[path = "recovery_sources_tests.rs"]
mod recovery_sources_tests;

#[path = "maintenance_tests.rs"]
mod maintenance_tests;

#[path = "original_cleanup_tests.rs"]
mod original_cleanup_tests;

#[path = "result_activation_budget_tests.rs"]
mod result_activation_budget_tests;

#[path = "startup_metadata_tests.rs"]
mod startup_metadata_tests;

#[path = "deletion_maintenance_tests.rs"]
mod deletion_maintenance_tests;
