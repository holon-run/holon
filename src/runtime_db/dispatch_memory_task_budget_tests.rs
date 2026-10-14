use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
    },
    runtime_db::{
        task_snapshot as tasks,
        transitions::{PreparedTaskTransition, TaskTransitionCommand},
    },
    types::{TaskKind, TaskRecord, TaskStatus},
};
fn task_request() -> tasks::TaskDomainLoadRequest {
    tasks::TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 32 * 1024 * 1024,
    }
}
fn memory_request(bytes: usize) -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: bytes,
    }
}
fn install_task(f: &Fixture) -> Result<TaskRecord> {
    let task = TaskRecord {
        id: "memory-budget-task".into(),
        agent_id: f.scope.agent_id.clone(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: f.at,
        updated_at: f.at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("x".repeat(1024 * 1024)),
        detail: None,
        recovery: None,
    };
    f.db.transaction(|tx| {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
        let proof = tasks::prepare_task_write_tx(tx, &f.scope, &keys)?;
        crate::runtime_db::repositories::upsert_task_tx(tx, &task)?;
        let state = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .state
            .unwrap();
        tasks::register_task_writes_tx(tx, &proof, &keys, &state, &task_request())?;
        Ok(())
    })?;
    Ok(task)
}
#[test]
fn task_preflight_combined_limit_precedes_malformed_body_decode_and_releases_guard() -> Result<()> {
    let f = Fixture::new(true)?;
    let task = install_task(&f)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let raw: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM tasks WHERE task_id=?1",
        [&task.id],
        |r| r.get(0),
    )?;
    f.db.connection()?.execute(
        "UPDATE tasks SET payload_json=?2 WHERE task_id=?1",
        params![task.id, "!".repeat(raw.len())],
    )?;
    f.db.transaction(|tx| {
        let proof = tasks::prepare_task_write_tx(tx, &f.scope, &BTreeSet::new())?;
        let memory = DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("task-preflight-budget"),
        };
        let mut budget = DispatchMemoryTransactionBudget::prepare_tx(
            tx,
            &memory,
            &memory_request(512 * 1024),
            &ledger,
        )?;
        let baseline = ledger.report()?.observed_bytes;
        assert!(baseline > 0);
        let error = tasks::preflight_task_write_snapshot_with_memory_budget_tx(
            tx,
            &proof,
            &task_request(),
            &ledger,
            Some(&mut budget),
        )
        .err()
        .unwrap();
        assert!(
            matches!(
                error.downcast_ref::<DispatchMemoryError>(),
                Some(DispatchMemoryError::ResourceExhausted)
            ),
            "{error:#}"
        );
        assert!(ledger.report()?.observed_bytes >= baseline);
        drop(budget);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })?;
    f.db.connection()?.execute(
        "UPDATE tasks SET payload_json=?2 WHERE task_id=?1",
        params![task.id, raw],
    )?;
    f.db.transaction(|tx| {
        let proof = tasks::prepare_task_write_tx(tx, &f.scope, &BTreeSet::new())?;
        let memory = DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("task-preflight-large"),
        };
        let mut budget = DispatchMemoryTransactionBudget::prepare_tx(
            tx,
            &memory,
            &memory_request(64 * 1024 * 1024),
            &ledger,
        )?;
        let snapshot = tasks::preflight_task_write_snapshot_with_memory_budget_tx(
            tx,
            &proof,
            &task_request(),
            &ledger,
            Some(&mut budget),
        )?
        .require_current()?;
        assert!(snapshot
            .resident
            .capture(&TaskCaptureRequest::current())?
            .lookup(&TaskKey::new(TaskFamily::Task, &task.id))?
            .is_some());
        drop(snapshot);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(budget);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
#[test]
fn original_task_material_combined_limit_precedes_proof_decode_and_business_sql() -> Result<()> {
    let f = Fixture::new(true)?;
    let task = install_task(&f)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let command = TaskTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        task: task.clone(),
        agent_deletion: None,
        task_result_settlement: None,
        queue_entry: None,
        work_items: vec![],
        expected_wait_conditions: vec![],
        wait_conditions: vec![],
        agent_state: None,
        message_evidence: vec![],
        audit_events: vec![],
        index_changes: vec![],
        notify_scheduler: false,
        commit_on_idempotent: false,
        fault: None,
    };
    let prepared = PreparedInputTransition::task(
        f.scope.clone(),
        "task-combined-before-proof".into(),
        f.at,
        PreparedTaskTransition::new(command, Default::default(), false, None, None)?,
    )?;
    let before: usize =
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |r| {
                r.get(0)
            })?;
    f.db.connection()?.execute(
        "UPDATE tasks SET payload_json=?2 WHERE task_id=?1",
        params![task.id, "!".repeat(1024 * 1024)],
    )?;
    let prepared = prepared.with_dispatch_memory(DispatchMemoryIntent::Deferred {
        binding: DispatchMemoryBinding::from_start(&f.start),
        frozen: f.frozen("task-proof-budget"),
    })?;
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &prepared,
                &memory_request(512 * 1024),
                &ledger,
                &task_request(),
            )
            .err()
            .unwrap();
    assert!(
        matches!(
            error.downcast_ref::<DispatchMemoryError>(),
            Some(DispatchMemoryError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |r| {
                r.get::<_, usize>(0)
            })?,
        before
    );
    assert_eq!(f.db.connection()?.query_row("SELECT lifecycle_state FROM execution_protocol_attempts WHERE attempt_id='no-such-new-attempt'",[],|r|r.get::<_,String>(0)).optional()?,None);
    Ok(())
}
