use super::*;

#[test]
fn actual_started_memory_releases_discarded_task_preflight_without_a_second_debit() -> Result<()> {
    use crate::runtime::resident_task::{TaskCaptureRequest, TaskFamily, TaskKey};
    use crate::runtime_db::task_snapshot::{self as tasks, read_preparation::TaskReadMaterial};
    let f = Fixture::new(false)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let policy = tasks::TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65_536,
    };
    let work = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65_536,
    };
    f.db.transaction(|tx| {
        let value = crate::types::TaskRecord {
            id: "actual-memory-preflight-task".into(),
            agent_id: f.scope.agent_id.clone(),
            kind: crate::types::TaskKind::CommandTask,
            status: crate::types::TaskStatus::Running,
            created_at: f.at,
            updated_at: f.at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("original Task".into()),
            detail: None,
            recovery: None,
        };
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &value.id)]);
        let proof = tasks::prepare_task_write_tx(tx, &f.scope, &keys)?;
        crate::runtime_db::repositories::upsert_task_tx(tx, &value)?;
        let state = ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .state
            .context("actual State")?;
        tasks::register_task_writes_tx(tx, &proof, &keys, &state, &policy)?;
        let intent = DispatchMemoryIntent::Started {
            start: Box::new(f.start.clone()),
        };
        let mut memory = DispatchMemoryTransactionBudget::prepare_tx(tx, &intent, &work, &ledger)?;
        let memory_prior = memory.material_usage_for_test();
        let mut phase = TaskReadMaterial::new(&f.scope, &policy, Some(&ledger))
            .with_memory_budget(Some(&mut memory));
        phase.charge_writer_material(1024, 4, 1)?;
        let proof = tasks::prepare_original_task_write_using_material_tx(
            tx, &f.scope, &keys, false, &policy, &ledger, &mut phase,
        )?;
        let admitted = phase.material_estimate();
        let baseline = ledger.report()?.observed_bytes;
        tasks::preflight_original_task_using_material_tx(tx, &proof, &policy, &ledger, &mut phase)?;
        assert_eq!(
            (
                phase.material_estimate().bytes,
                phase.material_estimate().records
            ),
            (admitted.bytes, admitted.records)
        );
        assert_eq!(
            ledger.report()?.observed_bytes,
            baseline,
            "actual preflight allocation retired at its synchronous drop"
        );
        let guards = phase.into_guards();
        assert_eq!(
            memory.material_usage_for_test(),
            (
                memory_prior.0 + admitted.bytes,
                memory_prior.1 + admitted.records
            ),
            "Task actual guards debit Memory once; retired scratch cannot remain in its counter"
        );
        drop((proof, memory));
        assert!(ledger.report()?.observed_bytes > 0);
        let last = guards.clone();
        drop(guards);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}
