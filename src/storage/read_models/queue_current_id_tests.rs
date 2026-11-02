use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    storage::AppStorage,
    types::WorkItemPlanStatus,
};
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-current-id".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

#[test]
fn queue_current_id_borrow_preserves_exact_open_and_terminal_selection() -> Result<()> {
    for id in ["".to_owned(), "current 汉字\0🦀".repeat(1024)] {
        for state in [
            WorkItemState::Open,
            WorkItemState::Completing,
            WorkItemState::Completed,
        ] {
            for selected in [None, Some("missing"), Some(id.as_str())] {
                let mut record = WorkItemRecord::new("web", "current selection", state.clone());
                record.id = id.clone();
                record.plan_status = WorkItemPlanStatus::Ready;
                let queue = derive_context_work_queue(
                    selected,
                    QueueLatestWork::from([(id.clone(), record)]),
                    vec![],
                    vec![],
                    vec![],
                )?;
                let is_current = selected == Some(id.as_str()) && state == WorkItemState::Open;
                assert_eq!(queue.current.is_some(), is_current);
                assert_eq!(queue.current_runnable.is_some(), is_current);
                assert_eq!(queue.items[0].is_current, is_current);
                assert_eq!(queue.items[0].work_item.id, id);
                assert_eq!(
                    queue.queued_blocked.len(),
                    usize::from(state == WorkItemState::Open && !is_current)
                );
            }
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn queue_current_id_empty_queue_adds_no_material_and_does_not_reset_refusal() -> Result<()> {
    let id = "missing 汉字\0🦀".repeat(8192);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let cost = header();
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        for selected in [None, Some(""), Some(id.as_str()), Some(id.as_str())] {
            let queue = derive_context_work_queue(
                selected,
                QueueLatestWork::new(),
                vec![],
                vec![],
                vec![],
            )?;
            assert!(queue.current.is_none());
            assert_eq!(queue.items.capacity(), 0);
            assert_eq!(phase.finish()?, cost);
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
        }
        assert!(charge_owned_metadata(1, 1).is_err());
        // No selector material is allocated even after the original phase fails.
        let queue =
            derive_context_work_queue(Some(&id), QueueLatestWork::new(), vec![], vec![], vec![])?;
        assert!(queue.items.is_empty());
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard().is_err());
    }
    assert_eq!(ledger.report()?.observed_bytes, cost.0);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, cost.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn queue_current_id_observed_reader_reuses_state_and_phase_on_reentry() -> Result<()> {
    let home = tempfile::tempdir()?;
    let storage = AppStorage::new_for_test_with_agent(home.path(), "web")?;
    let view = RuntimeReadModels::borrowed_for_agent(&storage.runtime_db, "web")?;
    let mut state = AgentState::new("web");
    state.current_work_item_id = Some("missing 汉字\0🦀".repeat(8192));
    let id = state.current_work_item_id.as_ref().unwrap();
    let original = (id.as_ptr(), id.capacity());
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let cost = header();
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        for _ in 0..2 {
            let queue = view.work_queue_with_observed_scheduling(&state, &[])?;
            assert!(queue.current.is_none());
            assert!(queue.items.is_empty());
            let id = state.current_work_item_id.as_ref().unwrap();
            assert_eq!((id.as_ptr(), id.capacity()), original);
            assert_eq!(phase.finish()?, cost);
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
        }
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, cost.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn queue_current_id_reader_preserves_scope_and_sql_error_propagation() -> Result<()> {
    let home = tempfile::tempdir()?;
    let storage = AppStorage::new_for_test_with_agent(home.path(), "web")?;
    let view = RuntimeReadModels::borrowed_for_agent(&storage.runtime_db, "web")?;
    let mut state = AgentState::new("other");
    state.current_work_item_id = Some("missing 汉字\0🦀".repeat(1024));
    assert_eq!(
        view.work_queue_with_observed_scheduling(&state, &[])
            .unwrap_err()
            .to_string(),
        "WorkQueue Scheduling observation scope differs"
    );
    state.id = "web".into();
    storage
        .runtime_db
        .connection()?
        .execute_batch("DROP TABLE work_items")?;
    let observed = view
        .work_queue_with_observed_scheduling(&state, &[])
        .unwrap_err();
    let ordinary = view.work_queue().unwrap_err();
    assert_eq!(format!("{observed:#}"), format!("{ordinary:#}"));
    assert!(format!("{observed:#}").contains("no such table: work_items"));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
