use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{
        CallbackDeliveryMode, ExternalTriggerRecord, ExternalTriggerScope, WakeSource,
        WorkItemContinuationFrame, WorkItemPlanStatus,
    },
    work_item_scheduling::{derive_work_item_scheduling, WorkItemSchedulingFacts},
};
use chrono::{DateTime, Utc};
use std::sync::Arc;

#[path = "queue_trigger_material_tests/observed_tests.rs"]
mod observed_tests;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-trigger".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn slots(count: usize) -> (u64, usize) {
    (
        (count * std::mem::size_of::<(String, DateTime<Utc>, usize)>()) as u64 * 8,
        count * 8,
    )
}

fn trigger(id: &str, delivered: Option<i64>, active: bool) -> ExternalTriggerRecord {
    let time: DateTime<Utc> = "2026-10-01T00:00:00Z".parse().unwrap();
    let mut id = id.to_owned();
    id.reserve(137);
    ExternalTriggerRecord {
        external_trigger_id: id,
        target_agent_id: "web".into(),
        scope: ExternalTriggerScope::Agent,
        delivery_mode: CallbackDeliveryMode::WakeHint,
        token: None,
        token_hash: "fixture-only".into(),
        status: if active {
            ExternalTriggerStatus::Active
        } else {
            ExternalTriggerStatus::Revoked
        },
        created_at: time,
        revoked_at: (!active).then_some(time),
        last_delivered_at: delivered.map(|seconds| time + chrono::Duration::seconds(seconds)),
        delivery_count: u64::from(delivered.is_some()),
    }
}

fn legacy(rows: &[ExternalTriggerRecord]) -> BTreeMap<String, DateTime<Utc>> {
    rows.iter()
        .filter(|row| row.status == ExternalTriggerStatus::Active)
        .filter_map(|row| {
            row.last_delivered_at
                .map(|time| (row.external_trigger_id.clone(), time))
        })
        .collect()
}

#[test]
fn trigger_slots_exact_cost_moves_ids_on_original_phase_and_holder() -> Result<()> {
    for count in [1, 8, 65] {
        let rows = (0..count)
            .rev()
            .map(|i| trigger(&format!("trigger-{i:03} 汉字\0🦀"), Some(i as i64), true))
            .collect::<Vec<_>>();
        let pointers = rows
            .iter()
            .map(|row| {
                let id = &row.external_trigger_id;
                (id.clone(), (id.as_ptr() as usize, id.capacity()))
            })
            .collect::<BTreeMap<_, _>>();
        let cost = (header().0 + slots(count).0, header().1 + slots(count).1);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
        let original = phase.guard()?;
        let index = {
            let _entry = phase.enter();
            let index = queue_trigger_deliveries(rows)?;
            assert!(Arc::ptr_eq(
                &original,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            index
        };
        assert_eq!(index.len(), count);
        assert!(index.capacity() >= count);
        assert!(index.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for (id, time, _) in &index {
            assert_eq!((id.as_ptr() as usize, id.capacity()), pointers[id]);
            assert_eq!(queue_trigger_delivery(&index, id), Some(*time));
        }
        assert_eq!(queue_trigger_delivery(&index, "absent"), None);
        assert_eq!(phase.finish()?, cost);
        let last = phase.guard()?;
        drop((phase, original));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(index);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn trigger_filtering_and_duplicate_ids_keep_last_effective_input_not_max_time() -> Result<()> {
    let rows = vec![
        trigger("same", Some(90), true),
        trigger("", Some(5), true),
        trigger("same", Some(20), true),
        trigger("汉字\0🦀", Some(60), true),
        trigger("same", Some(100), false),
        trigger("same", None, true),
        trigger("revoked", Some(999), false),
        trigger("undelivered", None, true),
    ];
    let expected = legacy(&rows);
    let last_id = (
        rows[2].external_trigger_id.as_ptr() as usize,
        rows[2].external_trigger_id.capacity(),
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let cost = (header().0 + slots(4).0, header().1 + slots(4).1);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), cost.0, cost.1)?;
    let actual = {
        let _entry = phase.enter();
        queue_trigger_deliveries(rows)?
    };
    assert_eq!(actual.len(), 3);
    for (id, time) in expected {
        assert_eq!(queue_trigger_delivery(&actual, &id), Some(time));
    }
    let same = &actual[actual
        .binary_search_by(|row| row.0.as_str().cmp("same"))
        .unwrap()];
    assert_eq!((same.0.as_ptr() as usize, same.0.capacity()), last_id);
    assert_eq!(same.2, 2);
    assert_eq!(queue_trigger_delivery(&actual, "revoked"), None);
    assert_eq!(queue_trigger_delivery(&actual, "undelivered"), None);
    assert_eq!(phase.finish()?, cost);
    drop((actual, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn trigger_empty_filtered_and_outside_phase_paths_allocate_no_index_slots() -> Result<()> {
    for rows in [
        vec![],
        vec![
            trigger("revoked", Some(1), false),
            trigger("pending", None, true),
        ],
    ] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
        {
            let _entry = phase.enter();
            let actual = queue_trigger_deliveries(rows)?;
            assert!(actual.is_empty());
            assert_eq!(actual.capacity(), 0);
            assert_eq!(queue_trigger_delivery(&actual, ""), None);
        }
        assert_eq!(phase.finish()?, header());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let actual = queue_trigger_deliveries(vec![trigger("outside", Some(1), true)])?;
    assert_eq!(actual.len(), 1);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn trigger_slot_byte_and_record_rejections_latch_without_partial_queue_or_phase_reset() -> Result<()>
{
    for count in [1, 65] {
        for bytes in [true, false] {
            let cost = (header().0 + slots(count).0, header().1 + slots(count).1);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes { cost.0 - 1 } else { cost.0 },
                if bytes { cost.1 } else { cost.1 - 1 },
            )?;
            let original = phase.guard()?;
            {
                let _entry = phase.enter();
                for _ in 0..2 {
                    let rows = (0..count)
                        .map(|_| trigger("duplicate", Some(1), true))
                        .collect();
                    let error = derive_context_work_queue(
                        None,
                        QueueLatestWork::new(),
                        rows,
                        vec![],
                        vec![],
                    )
                    .unwrap_err();
                    assert!(
                        error.to_string().contains("queue trigger slots admission"),
                        "{error:#}"
                    );
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
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
    }
    Ok(())
}

fn wait(index: usize, kind: WaitConditionKind) -> WaitConditionRecord {
    let time = "2026-10-01T00:00:00Z".parse().unwrap();
    WaitConditionRecord {
        id: format!("wait-{index}"),
        agent_id: "web".into(),
        work_item_id: Some("work".into()),
        status: WaitConditionStatus::Active,
        kind,
        source: None,
        subject_ref: None,
        waiting_for: "trigger-fixture".into(),
        wake_sources: vec![
            WakeSource::ExternalIngress {
                external_trigger_id: Some("same".into()),
            },
            WakeSource::ExternalIngress {
                external_trigger_id: Some("other".into()),
            },
            WakeSource::ExternalIngress {
                external_trigger_id: Some("missing".into()),
            },
            WakeSource::SystemTick,
        ],
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
}

#[test]
fn trigger_queue_matches_public_map_scheduling_matrix_and_wire() -> Result<()> {
    let rows = vec![
        trigger("same", Some(90), true),
        trigger("same", Some(20), true),
        trigger("other", Some(60), true),
        trigger("same", Some(999), false),
        trigger("same", None, true),
    ];
    let expected_index = legacy(&rows);
    for state in [
        WorkItemState::Open,
        WorkItemState::Completing,
        WorkItemState::Completed,
    ] {
        for current in [false, true] {
            for yielded in [false, true] {
                for blocked in [false, true] {
                    for needs_input in [false, true] {
                        for waits in [
                            vec![],
                            vec![wait(0, WaitConditionKind::External)],
                            vec![
                                wait(1, WaitConditionKind::Task),
                                wait(2, WaitConditionKind::Operator),
                            ],
                        ] {
                            let mut item =
                                WorkItemRecord::new("web", "trigger matrix", state.clone());
                            item.id = "work".into();
                            item.plan_status = if needs_input {
                                WorkItemPlanStatus::NeedsInput
                            } else {
                                WorkItemPlanStatus::Ready
                            };
                            item.blocked_by = blocked.then(|| "manual".into());
                            let is_current = current && state == WorkItemState::Open;
                            let is_yielded = yielded && state == WorkItemState::Open;
                            let expected = derive_work_item_scheduling(WorkItemSchedulingFacts {
                                work_item: &item,
                                is_current,
                                is_yielded,
                                active_wait_conditions: &waits,
                                trigger_delivery_by_id: &expected_index,
                            });
                            let frames = if yielded {
                                vec![WorkItemContinuationFrame::new_on_completed(
                                    "web", "work", "other", None,
                                )]
                            } else {
                                vec![]
                            };
                            let observed = derive_observed_context_work_queue(
                                current.then_some("work"),
                                QueueLatestWork::from([("work".into(), item.clone())]),
                                &rows,
                                waits.clone(),
                                frames.clone(),
                            )?;
                            let queue = derive_context_work_queue(
                                current.then_some("work"),
                                QueueLatestWork::from([("work".into(), item)]),
                                rows.clone(),
                                waits.clone(),
                                frames,
                            )?;
                            assert_eq!(observed.current, queue.current);
                            assert_eq!(observed.queued_blocked, queue.queued_blocked);
                            assert_eq!(observed.items, queue.items);
                            assert_eq!(observed.current_runnable, queue.current_runnable);
                            assert_eq!(observed.triggered_blocked, queue.triggered_blocked);
                            assert_eq!(observed.queued_runnable, queue.queued_runnable);
                            assert_eq!(observed.yielded, queue.yielded);
                            assert_eq!(observed.waiting_for_operator, queue.waiting_for_operator);
                            assert_eq!(observed.blocked, queue.blocked);
                            assert_eq!(observed.completing, queue.completing);
                            assert_eq!(observed.completed_recent, queue.completed_recent);
                            assert_eq!(queue.items, [expected.clone()]);
                            assert_eq!(
                                serde_json::to_value(&queue.items[0])?,
                                serde_json::to_value(expected)?
                            );
                            assert_eq!(queue.items[0].has_triggered_waits, !waits.is_empty());
                            assert_eq!(
                                queue.items[0].last_triggered_at,
                                (!waits.is_empty()).then(|| {
                                    "2026-10-01T00:01:00Z".parse::<DateTime<Utc>>().unwrap()
                                })
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
