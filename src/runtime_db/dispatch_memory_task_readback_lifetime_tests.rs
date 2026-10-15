use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey};
use crate::runtime_db::task_snapshot as tasks;
use crate::types::{TaskKind, TaskRecord, TaskStatus};

#[test]
fn actual_started_memory_task_readback_retires_only_its_dead_suffix_and_borrows_retained_guards_once(
) -> Result<()> {
    let f = Fixture::new(true)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let request = tasks::TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    };
    f.db.transaction(|tx| {
        let state = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .state
            .context("actual M2 State absent")?;
        let record = TaskRecord {
            id: "actual-memory-readback-task".into(),
            agent_id: f.scope.agent_id.clone(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: f.at,
            updated_at: f.at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({"real_values":[0,1,2,3,4,5,6,7,8,9]})),
            recovery: None,
        };
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &record.id)]);
        let intent = DispatchMemoryIntent::Started {
            start: Box::new(f.start.clone()),
        };
        let mut memory =
            DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &policy(), &ledger)?;
        let before = memory.material_usage_for_test();
        let mut phase =
            tasks::read_preparation::TaskReadMaterial::new(&f.scope, &request, Some(&ledger))
                .with_memory_budget(Some(&mut memory));
        phase.charge_writer_material(1024, 4, 1)?;
        let proof = tasks::prepare_original_task_write_using_material_tx(
            tx, &f.scope, &keys, false, &request, &ledger, &mut phase,
        )?;
        crate::runtime_db::repositories::upsert_task_tx(tx, &record)?;
        let facts = tasks::register_original_task_writes_using_material_tx(
            tx, &proof, &keys, &state, &request, &ledger, &mut phase,
        )?;
        let actual = phase.material_estimate();
        let views = phase.into_guards();
        assert_eq!(
            memory.material_usage_for_test(),
            (before.0 + actual.bytes, before.1 + actual.records),
            "dead scratch retirement cannot double-debit or forget the actual retained M2 material"
        );
        assert_eq!(facts.delta.records.len(), 1);
        let last = facts.delta.clone();
        let last_views = views.clone();
        drop((facts, views, proof, memory));
        assert!(ledger.report()?.observed_bytes > 0);
        drop(last);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(last_views);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
fn policy() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    }
}
