use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{TodoItem, TodoItemState},
    work_item_scheduling::{
        derive_work_item_scheduling, WorkItemSchedulingFacts, WorkItemSchedulingProjection,
    },
};
use std::borrow::Cow;

fn projection(item: &WorkItemRecord) -> WorkItemSchedulingProjection {
    derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: item,
        is_current: false,
        is_yielded: false,
        active_wait_conditions: &[],
        trigger_delivery_by_id: &std::collections::BTreeMap::new(),
    })
}

#[test]
fn borrows_short_fields_and_preserves_unicode_preview() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "state-dto".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let mut item = WorkItemRecord::new(&scope.agent_id, "短文汉🦀", WorkItemState::Open);
    item.todo_list = vec![TodoItem {
        text: "next 汉🦀".into(),
        state: TodoItemState::InProgress,
    }];
    let projection = projection(&item);
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let header = phase.finish()?;
    let dto = phase.read(|| slim_state_work_item_dto(&projection))?;
    assert_eq!(phase.finish()?, header);
    assert!(matches!(dto.id, Cow::Borrowed(_)));
    assert!(matches!(dto.objective, Cow::Borrowed(_)));
    assert_eq!(dto.id.as_ptr(), projection.record().id.as_ptr());
    assert!(std::ptr::eq(
        dto.current_todo.unwrap(),
        projection.current_todo.as_ref().unwrap(),
    ));
    assert_eq!(
        serde_json::to_value(&dto)?,
        serde_json::to_value(crate::http_dto::SlimWorkItemDto::from(&projection))?
    );
    drop(dto);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);

    let unicode = "汉🦀é".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT);
    for limit in [0, 1, 2, 3, 4, 5, STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT] {
        let expected = if limit <= 3 {
            unicode.chars().take(limit).collect::<String>()
        } else {
            format!("{}...", unicode.chars().take(limit - 3).collect::<String>())
        };
        assert_eq!(truncate_state_bootstrap_string(&unicode, limit), expected);
    }
    Ok(())
}

#[test]
fn owned_preview_and_slots_reject_each_short_axis() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "state-dto-budget".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let item = WorkItemRecord::new(
        &scope.agent_id,
        "汉🦀".repeat(STATE_BOOTSTRAP_TEXT_PREVIEW_LIMIT),
        WorkItemState::Open,
    );
    let items = [projection(&item)];
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let dto = phase.read(|| slim_state_work_items(&items))?;
    let cost = phase.finish()?;
    assert!(matches!(dto[0].objective, Cow::Owned(_)));
    drop(dto);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for (bytes, records) in [cost, (cost.0 - 1, cost.1), (cost.0, cost.1 - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
        let result = phase.read(|| slim_state_work_items(&items));
        if (bytes, records) == cost {
            assert_eq!(result?.len(), 1);
            assert_eq!(phase.finish()?, cost);
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            assert!(phase.read(|| Ok(())).is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn known_display_window_reserves_only_selected_slots() -> Result<()> {
    let scope = InputEvidenceScope {
        agent_id: "state-dto-window".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let items = (0..200)
        .map(|index| {
            projection(&WorkItemRecord::new(
                &scope.agent_id,
                format!("window item {index}"),
                WorkItemState::Open,
            ))
        })
        .collect::<Vec<_>>();
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let header = phase.finish()?;
    drop(phase);
    let count = STATE_BOOTSTRAP_WORK_ITEM_LIMIT;
    let cost = (
        header.0 + u64::try_from(count * std::mem::size_of::<StateWorkItemDto<'_>>())? * 8,
        header.1 + count * 8,
    );
    let expected = serde_json::to_value(
        items
            .iter()
            .take(count)
            .map(crate::http_dto::SlimWorkItemDto::from)
            .collect::<Vec<_>>(),
    )?;
    for (bytes, records) in [cost, (cost.0 - 1, cost.1), (cost.0, cost.1 - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
        let result = phase.read(|| slim_state_work_items(&items));
        if (bytes, records) == cost {
            let dto = result?;
            assert_eq!(dto.len(), count);
            assert_eq!(dto.capacity(), count);
            assert_eq!(serde_json::to_value(&dto)?, expected);
            assert_eq!(phase.finish()?, cost);
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            assert!(phase.read(|| Ok(())).is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
