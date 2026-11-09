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
        recovery_at: None,
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
fn task_preflight_rejects_malformed_body_without_quota_and_releases_guard() -> Result<()> {
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
            !matches!(
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
fn original_task_material_rejects_corruption_without_quota_and_business_writes() -> Result<()> {
    let f = Fixture::new(true)?;
    let task = install_task(&f)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let command = TaskTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        expected_owner_incarnation: f.scope.identity_incarnation,
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
        !matches!(
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

#[test]
fn task_material_shares_its_owner_with_memory_without_duplicate_charge() -> Result<()> {
    let f = Fixture::new(true)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("shared-task-material"),
        };
        let mut memory = DispatchMemoryTransactionBudget::prepare_tx(
            tx,
            &intent,
            &memory_request(64 * 1024 * 1024),
            &ledger,
        )?;
        let baseline = memory.input_material_usage();
        let observed = ledger.report()?.observed_bytes;
        let request = task_request();
        let mut standalone =
            tasks::read_preparation::TaskReadMaterial::new(&f.scope, &request, Some(&ledger));
        standalone.charge(4096, 100, 2, false)?;
        let task_observed = ledger.report()?.observed_bytes - observed;
        drop(standalone);
        assert_eq!(ledger.report()?.observed_bytes, observed);
        let mut material =
            tasks::read_preparation::TaskReadMaterial::new(&f.scope, &request, Some(&ledger))
                .with_memory_budget(Some(&mut memory));
        material.charge(4096, 100, 2, false)?;
        material.charge(1024, 4, 1, true)?;
        drop(material);
        assert_eq!(
            memory.input_material_usage(),
            (baseline.0 + 9216, baseline.1 + 204)
        );
        assert_eq!(
            ledger.report()?.observed_bytes - observed,
            task_observed + 1024
        );
        // Memory retains the original Task owner through the outer transaction.
        drop(memory);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn task_material_ignores_size_hint_but_rejects_foreign_ledger_and_scope() -> Result<()> {
    let f = Fixture::new(true)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    f.db.transaction(|tx| {
        let intent = DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("task-material-boundary"),
        };
        let baseline = {
            let memory = DispatchMemoryTransactionBudget::prepare_tx(
                tx,
                &intent,
                &memory_request(64 * 1024 * 1024),
                &ledger,
            )?;
            memory.input_material_usage().0
        };
        let request = task_request();
        let mut memory = DispatchMemoryTransactionBudget::prepare_tx(
            tx,
            &intent,
            &memory_request(baseline + 8191),
            &ledger,
        )?;
        let observed = ledger.report()?.observed_bytes;
        let mut material =
            tasks::read_preparation::TaskReadMaterial::new(&f.scope, &request, Some(&ledger))
                .with_memory_budget(Some(&mut memory));
        material.charge(4096, 1, 2, false)?;
        drop(material);
        let charged = ledger.report()?.observed_bytes;
        assert!(charged > observed);
        let observed = charged;
        let foreign = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let mut material =
            tasks::read_preparation::TaskReadMaterial::new(&f.scope, &request, Some(&foreign))
                .with_memory_budget(Some(&mut memory));
        assert!(material.charge(1, 1, 1, false).is_err());
        drop(material);
        assert_eq!(foreign.report()?.observed_bytes, 0);
        let mut wrong_scope = f.scope.clone();
        wrong_scope.identity_incarnation += 1;
        let mut material =
            tasks::read_preparation::TaskReadMaterial::new(&wrong_scope, &request, Some(&ledger))
                .with_memory_budget(Some(&mut memory));
        assert!(material.charge(1, 1, 1, false).is_err());
        drop(material);
        assert_eq!(ledger.report()?.observed_bytes, observed);
        drop(memory);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn in_place_task_output_metadata_reuses_admission_and_charges_growth() -> Result<()> {
    let f = Fixture::new(true)?;
    install_task(&f)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    f.db.transaction(|tx| {
        let request = task_request();
        let mut material =
            tasks::read_preparation::TaskReadMaterial::new(&f.scope, &request, Some(&ledger));
        let mut coverage = material
            .read_current_metadata(tx)?
            .context("Task coverage missing")?;
        let baseline = material.material_estimate();
        material.charge_reused_current_metadata(&coverage)?;
        assert_eq!(material.material_estimate().bytes, baseline.bytes);
        assert_eq!(material.material_estimate().records, baseline.records);
        let encoded_before = serde_json::to_vec(&coverage)?.len();
        let records_before = coverage.owned_metadata_record_count()?;
        coverage
            .output
            .window
            .get_or_insert(crate::runtime::resident_task::TaskOutputWindowObservation {
                message_ids: Vec::new(),
                is_exhaustive: true,
            })
            .message_ids
            .push("new-output-source".into());
        let byte_growth = serde_json::to_vec(&coverage)?.len() - encoded_before;
        let record_growth = coverage.owned_metadata_record_count()? - records_before;
        material.charge_reused_current_metadata(&coverage)?;
        assert_eq!(
            material.material_estimate().bytes,
            baseline.bytes + byte_growth * 4
        );
        assert_eq!(
            material.material_estimate().records,
            baseline.records + record_growth * 4
        );
        drop(material);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let mut unadmitted =
            tasks::read_preparation::TaskReadMaterial::new(&f.scope, &request, Some(&ledger));
        assert!(unadmitted
            .charge_reused_current_metadata(&coverage)
            .is_err());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
