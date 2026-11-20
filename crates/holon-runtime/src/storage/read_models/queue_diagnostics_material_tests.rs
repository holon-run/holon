use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{WakeSource, WorkItemPlanStatus},
    work_item_scheduling::{
        derive_queue_work_item_scheduling, derive_work_item_scheduling, WorkItemSchedulingFacts,
    },
};
use std::{cell::Cell, sync::Arc};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-diagnostics".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn cost(messages: &[&str]) -> (u64, usize) {
    (
        (messages.len() * std::mem::size_of::<String>()
            + messages.iter().map(|text| text.len()).sum::<usize>()) as u64
            * 8,
        messages.len() * 2 * 8,
    )
}

fn item(state: WorkItemState) -> WorkItemRecord {
    let mut item = WorkItemRecord::new("web", "diagnostics 汉字 🦀", state);
    item.id = "work_diagnostics".into();
    item.plan_status = WorkItemPlanStatus::Ready;
    item
}

fn waits(mask: usize) -> Vec<WaitConditionRecord> {
    [
        WaitConditionKind::Task,
        WaitConditionKind::External,
        WaitConditionKind::Operator,
        WaitConditionKind::Timer,
        WaitConditionKind::System,
    ]
    .into_iter()
    .enumerate()
    .filter(|(n, _)| mask & (1 << n) != 0)
    .map(|(n, kind)| {
        let time = "2026-10-01T00:00:00Z".parse().unwrap();
        WaitConditionRecord {
            id: format!("wait-{n}"),
            agent_id: "web".into(),
            work_item_id: Some("work_diagnostics".into()),
            status: WaitConditionStatus::Active,
            kind,
            source: None,
            subject_ref: None,
            waiting_for: "diagnostic wait".into(),
            wake_sources: vec![WakeSource::ExternalIngress {
                external_trigger_id: Some("trigger-diagnostic".into()),
            }],
            continuation: None,
            created_at: time,
            updated_at: time,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        }
    })
    .collect()
}

#[test]
fn diagnostics_match_independent_conditions_order_and_public_api_matrix() -> Result<()> {
    for state in [
        WorkItemState::Open,
        WorkItemState::Completing,
        WorkItemState::Completed,
    ] {
        for current in [false, true] {
            for yielded in [false, true] {
                for mask in 0usize..32 {
                    let record = item(state.clone());
                    let mut conditions = waits(mask);
                    if let Some(first) = conditions.first().cloned() {
                        conditions.push(first);
                    }
                    let mut foreign = waits(31);
                    for wait in &mut foreign {
                        wait.work_item_id = Some("unrelated".into());
                    }
                    conditions.extend(foreign);
                    let mut expected = Vec::new();
                    if state != WorkItemState::Open && current {
                        expected.push("terminalizing_work_item_marked_current");
                    }
                    if yielded && current {
                        expected.push("yielded_work_item_marked_current");
                    }
                    if state != WorkItemState::Open && mask != 0 {
                        expected.push("terminalizing_work_item_has_active_wait");
                    }
                    if mask.count_ones() > 1 {
                        expected.push("multiple_active_wait_kinds");
                    }
                    let delivered = "2026-10-02T00:00:00Z".parse()?;
                    let deliveries = BTreeMap::from([("trigger-diagnostic".into(), delivered)]);
                    let public = derive_work_item_scheduling(WorkItemSchedulingFacts {
                        work_item: &record,
                        is_current: current,
                        is_yielded: yielded,
                        active_wait_conditions: &conditions,
                        trigger_delivery_by_id: &deliveries,
                    });
                    let actual = derive_queue_work_item_scheduling(
                        record,
                        current,
                        yielded,
                        &conditions,
                        &|id| deliveries.get(id).copied(),
                    )?;
                    assert_eq!(actual.diagnostics, expected);
                    assert_eq!(actual.diagnostics.capacity(), expected.len());
                    assert_eq!(actual, public);
                    assert_eq!(
                        serde_json::to_value(&actual)?,
                        serde_json::to_value(&public)?
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn diagnostics_exact_cost_and_original_record_survive_until_last_holder() -> Result<()> {
    let all = [
        "terminalizing_work_item_marked_current",
        "yielded_work_item_marked_current",
        "terminalizing_work_item_has_active_wait",
        "multiple_active_wait_kinds",
    ];
    for count in 1..=4 {
        let conditions = waits(if count == 4 {
            3
        } else if count == 3 {
            1
        } else {
            0
        });
        let record = item(WorkItemState::Completed);
        let pointer = record.objective.as_ptr();
        let added = cost(&all[..count]);
        let summaries = super::queue_wait_summary_material_tests::cost(&conditions)?;
        let total = (
            header().0 + added.0 + summaries.0,
            header().1 + added.1 + summaries.1,
        );
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
        let original = phase.guard()?;
        let projection = {
            let _entry = phase.enter();
            let row =
                derive_queue_work_item_scheduling(record, true, count > 1, &conditions, &|_| None)?;
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
            row
        };
        assert_eq!(projection.diagnostics, all[..count]);
        assert_eq!(projection.diagnostics.capacity(), count);
        assert!(projection
            .diagnostics
            .iter()
            .all(|s| s.capacity() == s.len()));
        assert_eq!(projection.objective.as_ptr(), pointer);
        assert_eq!(phase.finish()?, total);
        drop((projection, phase));
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn diagnostics_one_short_rejects_before_owning_derivation_and_latches_phase() -> Result<()> {
    let messages = [
        "terminalizing_work_item_marked_current",
        "yielded_work_item_marked_current",
        "terminalizing_work_item_has_active_wait",
        "multiple_active_wait_kinds",
    ];
    let added = cost(&messages);
    let total = (header().0 + added.0, header().1 + added.1);
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            if bytes { total.0 - 1 } else { total.0 },
            if bytes { total.1 } else { total.1 - 1 },
        )?;
        let original = phase.guard()?;
        let calls = Cell::new(0);
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
            for _ in 0..2 {
                let error = derive_queue_work_item_scheduling(
                    item(WorkItemState::Completed),
                    true,
                    true,
                    &waits(31),
                    &|_| {
                        calls.set(calls.get() + 1);
                        None
                    },
                )
                .unwrap_err();
                assert_eq!(error.to_string(), "queue diagnostics admission");
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(calls.get(), 0);
                assert_eq!(ledger.report()?.observed_bytes, header().0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, header().0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn no_diagnostics_costs_nothing_even_with_foreign_waits_or_failed_phase() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    let mut foreign = waits(31);
    for wait in &mut foreign {
        wait.work_item_id = None;
    }
    {
        let _entry = phase.enter();
        for failed in [false, true] {
            if failed {
                assert!(resource_io::charge_owned_metadata(1, 0).is_err());
            }
            let projection = derive_queue_work_item_scheduling(
                item(WorkItemState::Open),
                false,
                false,
                &foreign,
                &|_| None,
            )?;
            assert!(projection.diagnostics.is_empty());
            assert_eq!(projection.diagnostics.capacity(), 0);
            assert_eq!(ledger.report()?.observed_bytes, header().0);
            if failed {
                assert!(phase.finish().is_err());
            } else {
                assert_eq!(phase.finish()?, header());
            }
        }
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn shared_queue_diagnostics_rejection_returns_no_partial_result() -> Result<()> {
    let slots = (
        std::mem::size_of::<WorkItemSchedulingProjection>() as u64 * 8,
        8,
    );
    let added = cost(&["multiple_active_wait_kinds"]);
    let prefix = (header().0 + slots.0, header().1 + slots.1);
    let total = (prefix.0 + added.0, prefix.1 + added.1);
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            if bytes { total.0 - 1 } else { total.0 },
            if bytes { total.1 } else { total.1 - 1 },
        )?;
        let original = phase.guard()?;
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
            for attempt in 0..2 {
                let record = item(WorkItemState::Open);
                let error = derive_context_work_queue(
                    None,
                    QueueLatestWork::from([(record.id.clone(), record)]),
                    vec![],
                    waits(3),
                    vec![],
                )
                .unwrap_err();
                assert_eq!(
                    error.to_string(),
                    if attempt == 0 {
                        "queue diagnostics admission"
                    } else {
                        "queue items slots admission"
                    }
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, prefix.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
