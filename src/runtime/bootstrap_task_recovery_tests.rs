use super::*;
use crate::{
    runtime::resident_task::{CanonicalTaskFact, TaskCaptureRequest, TaskFamily, TaskKey},
    runtime_db::{
        context_inputs::ContextDurableRead,
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        repositories,
        task_snapshot::{
            prepare_task_domain_snapshot_tx, prepare_task_write_tx, register_task_writes_tx,
            TaskDomainLoadRequest,
        },
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        TaskKind,
    },
};
use chrono::Utc;

fn load() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn value(id: &str, status: TaskStatus, nanos: i64) -> TaskRecord {
    let at: chrono::DateTime<Utc> = "2026-10-01T00:00:00.123Z".parse().unwrap();
    TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status,
        created_at: at,
        updated_at: at + chrono::Duration::nanoseconds(nanos),
        parent_message_id: None,
        work_item_id: None,
        summary: Some(id.into()),
        detail: Some(serde_json::json!({"original":"snapshot"})),
        recovery: None,
    }
}
fn fixture() -> Result<(
    tempfile::TempDir,
    RuntimeDb,
    super::super::input_evidence_budget::InputEvidenceBudgetLedger,
    BootstrapTaskRecovery,
    Vec<TaskRecord>,
)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let ledger =
        super::super::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let scope = super::super::resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: 1,
    };
    let (recovery, expected) = db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                "default",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        let state = AgentState::new("default");
        upsert_agent_state_tx(tx, &state)?;
        prepare_task_domain_snapshot_tx(tx, &scope, &load())?.require_current()?;
        let rows = [
            value("b-running", TaskStatus::Running, 900),
            value("a-queued", TaskStatus::Queued, 100),
            value("c-cancelling", TaskStatus::Cancelling, 200),
            value("d-terminal", TaskStatus::Completed, 0),
        ];
        let keys = rows
            .iter()
            .map(|t| TaskKey::new(TaskFamily::Task, &t.id))
            .collect();
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        for row in &rows {
            repositories::upsert_task_tx(tx, row)?;
        }
        register_task_writes_tx(tx, &proof, &keys, &state, &load())?;
        let resident = prepare_task_domain_snapshot_tx(tx, &scope, &load())?
            .require_current()?
            .resident
            .with_budget(ledger.clone())?;
        let expected = ContextDurableRead::new(tx, "default").active_tasks("default")?;
        Ok((
            BootstrapTaskRecovery {
                selected: resident.capture(&super::super::task_reads::active_task_request())?,
                record_limit: load().record_limit,
                byte_limit: load().byte_limit,
            },
            expected,
        ))
    })?;
    Ok((home, db, ledger, recovery, expected))
}

#[test]
fn original_boot_task_capture_keeps_shared_bodies_and_sql_order_without_task_table() -> Result<()> {
    let (_home, db, ledger, recovery, expected) = fixture()?;
    let task = recovery
        .selected
        .records
        .iter()
        .find(|r| matches!(&r.fact,CanonicalTaskFact::Task(t) if t.id=="b-running"))
        .unwrap();
    let weak = Arc::downgrade(task);
    let before = ledger.report()?.owned.payload_bytes;
    db.transaction(|tx| {
        tx.execute_batch("DROP TABLE task_result_settlements; DROP TABLE tasks;")?;
        Ok(())
    })?;
    let (rows, retained) = recovery.into_records(&ledger)?;
    assert_eq!(rows, expected);
    assert_eq!(
        rows.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        vec!["a-queued", "b-running", "c-cancelling"]
    );
    assert!(ledger.report()?.owned.payload_bytes > before);
    assert!(
        weak.upgrade().is_some(),
        "only selected Arcs retain the frozen body"
    );
    drop(rows);
    drop(retained);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.owned.payload_bytes, 0);
    Ok(())
}

#[test]
fn original_boot_task_capture_does_not_follow_a_later_qualified_terminal_write() -> Result<()> {
    let (_home, db, ledger, recovery, expected) = fixture()?;
    let mut changed = expected
        .iter()
        .find(|t| t.id == "b-running")
        .unwrap()
        .clone();
    changed.status = TaskStatus::Completed;
    changed.updated_at += chrono::Duration::seconds(1);
    let scope = recovery.selected.scope.clone();
    db.transaction(|tx| {
        let key = TaskKey::new(TaskFamily::Task, &changed.id);
        let keys = std::collections::BTreeSet::from([key]);
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        repositories::upsert_task_tx(tx, &changed)?;
        register_task_writes_tx(tx, &proof, &keys, &AgentState::new("default"), &load())?;
        Ok(())
    })?;
    let (rows, _retained) = recovery.into_records(&ledger)?;
    assert_eq!(rows, expected);
    assert_eq!(
        rows.iter().find(|t| t.id == changed.id).unwrap().status,
        TaskStatus::Running
    );
    assert_eq!(
        db.tasks().latest(&changed.id)?.unwrap().status,
        TaskStatus::Completed
    );
    Ok(())
}

#[test]
fn original_boot_task_copy_budget_rejects_before_cloning_and_releases_source_pins() -> Result<()> {
    let (_home, _db, ledger, mut recovery, _expected) = fixture()?;
    let weak = Arc::downgrade(recovery.selected.records.first().unwrap());
    recovery.byte_limit = 1;
    let error = match recovery.into_records(&ledger) {
        Ok(_) => anyhow::bail!("tiny copy budget accepted"),
        Err(e) => e,
    };
    assert!(matches!(
        error.downcast_ref::<super::super::resident_task::TaskReadError>(),
        Some(super::super::resident_task::TaskReadError::ResourceExhausted(_))
    ));
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.owned.payload_bytes, 0);
    Ok(())
}
