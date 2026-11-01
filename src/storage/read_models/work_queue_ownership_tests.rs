use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{
        CallbackDeliveryMode, ExternalTriggerRecord, ExternalTriggerScope, TodoItem, TodoItemState,
        WakeSource, WorkItemContinuationFrame, WorkItemPlanStatus, WorkItemRef, WorkItemRefKind,
        WorkItemRefStatus,
    },
    work_item_scheduling::{
        derive_owned_work_item_scheduling, derive_work_item_scheduling, WorkItemSchedulingFacts,
        WorkItemSchedulingProjection,
    },
};
use std::sync::Arc;

fn record(id: &str, state: WorkItemState) -> WorkItemRecord {
    let mut item = WorkItemRecord::new("web", "queue 汉字 \0 \"🦀\"", state);
    item.id = id.into();
    item.plan_status = WorkItemPlanStatus::Ready;
    item.created_at = "2026-10-01T00:00:00Z".parse().unwrap();
    item.updated_at = item.created_at;
    item.todo_list = vec![
        TodoItem {
            text: "pending 汉字".into(),
            state: TodoItemState::Pending,
        },
        TodoItem {
            text: "in progress 🦀".into(),
            state: TodoItemState::InProgress,
        },
    ];
    item.work_refs = vec![WorkItemRef {
        kind: WorkItemRefKind::File,
        ref_id: "notes/汉字.md".into(),
        title: Some("title 🦀".into()),
        reason: "retained evidence".into(),
        status: WorkItemRefStatus::Active,
        last_seen_at: item.created_at,
        source_ref: Some("source:owned".into()),
        metadata: serde_json::Map::from_iter([(
            "nested".into(),
            serde_json::json!({"text": "nested 汉字", "array": ["🦀"]}),
        )]),
    }];
    // Spare capacity distinguishes a move from a clone even after allocator reuse.
    item.id.reserve(111);
    item.objective.reserve(137);
    item.todo_list.reserve(7);
    item.todo_list[1].text.reserve(99);
    item.work_refs.reserve(5);
    item.work_refs[0].ref_id.reserve(121);
    item
}

fn ownership(item: &WorkItemRecord) -> [(usize, usize); 8] {
    [
        (item.id.as_ptr() as usize, item.id.capacity()),
        (item.agent_id.as_ptr() as usize, item.agent_id.capacity()),
        (
            item.workspace_id.as_ptr() as usize,
            item.workspace_id.capacity(),
        ),
        (item.objective.as_ptr() as usize, item.objective.capacity()),
        (item.todo_list.as_ptr() as usize, item.todo_list.capacity()),
        (
            item.todo_list[1].text.as_ptr() as usize,
            item.todo_list[1].text.capacity(),
        ),
        (item.work_refs.as_ptr() as usize, item.work_refs.capacity()),
        (
            item.work_refs[0].ref_id.as_ptr() as usize,
            item.work_refs[0].ref_id.capacity(),
        ),
    ]
}

fn wait(item: &WorkItemRecord, kind: WaitConditionKind) -> WaitConditionRecord {
    WaitConditionRecord {
        id: format!("wait-{}", item.id),
        agent_id: item.agent_id.clone(),
        work_item_id: Some(item.id.clone()),
        status: WaitConditionStatus::Active,
        kind,
        source: Some("test".into()),
        subject_ref: Some("task-fixture".into()),
        waiting_for: "queue fixture".into(),
        wake_sources: vec![WakeSource::SystemTick],
        continuation: None,
        created_at: item.created_at,
        updated_at: item.updated_at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    }
}

fn borrowed(
    item: &WorkItemRecord,
    current: bool,
    yielded: bool,
    waits: &[WaitConditionRecord],
) -> WorkItemSchedulingProjection {
    derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: item,
        is_current: current,
        is_yielded: yielded,
        active_wait_conditions: waits,
        trigger_delivery_by_id: &BTreeMap::new(),
    })
}

#[test]
fn owned_scheduling_moves_nested_record_and_preserves_borrowed_independence() {
    let original = record("current", WorkItemState::Open);
    let expected = borrowed(&original, true, false, &[]);
    assert_ne!(expected.work_item.id.as_ptr(), original.id.as_ptr());
    assert_ne!(
        expected.work_item.todo_list.as_ptr(),
        original.todo_list.as_ptr()
    );
    let allocation = ownership(&original);
    let actual = derive_owned_work_item_scheduling(original, true, false, &[], &|_| None);
    assert_eq!(actual, expected);
    assert_eq!(ownership(&actual.work_item), allocation);
    assert_eq!(
        actual.current_todo,
        Some(actual.work_item.todo_list[1].clone())
    );
    assert_ne!(
        actual.current_todo.as_ref().unwrap().text.as_ptr(),
        actual.work_item.todo_list[1].text.as_ptr()
    );
}

#[test]
fn owned_and_borrowed_scheduling_match_complete_state_wait_and_focus_matrix() -> Result<()> {
    for state in [
        WorkItemState::Open,
        WorkItemState::Completing,
        WorkItemState::Completed,
    ] {
        for current in [false, true] {
            for yielded in [false, true] {
                for needs_input in [false, true] {
                    for blocked in [false, true] {
                        for kind in [
                            None,
                            Some(WaitConditionKind::Task),
                            Some(WaitConditionKind::External),
                            Some(WaitConditionKind::Operator),
                            Some(WaitConditionKind::Timer),
                            Some(WaitConditionKind::System),
                        ] {
                            let mut item = record("matrix", state.clone());
                            if needs_input {
                                item.plan_status = WorkItemPlanStatus::NeedsInput;
                            }
                            item.blocked_by = blocked.then(|| "manual blocker".into());
                            let waits = kind
                                .map(|kind| wait(&item, kind))
                                .into_iter()
                                .collect::<Vec<_>>();
                            let expected = borrowed(&item, current, yielded, &waits);
                            let allocation = ownership(&item);
                            let actual = derive_owned_work_item_scheduling(
                                item,
                                current,
                                yielded,
                                &waits,
                                &|_| None,
                            );
                            assert_eq!(actual, expected);
                            assert_eq!(ownership(&actual.work_item), allocation);
                            assert_eq!(
                                serde_json::to_value(&actual)?,
                                serde_json::to_value(&expected)?
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn ids(items: &[WorkItemSchedulingProjection]) -> Vec<&str> {
    items
        .iter()
        .map(|item| item.work_item.id.as_str())
        .collect()
}

#[test]
fn context_queue_moves_every_record_and_preserves_display_order_and_class_windows() {
    let mut records = vec![record("current", WorkItemState::Open)];
    let delivered_at = records[0].created_at;
    let deliveries = BTreeMap::from([("trigger-fixture".into(), delivered_at)]);
    let trigger = ExternalTriggerRecord {
        external_trigger_id: "trigger-fixture".into(),
        target_agent_id: "web".into(),
        scope: ExternalTriggerScope::Agent,
        delivery_mode: CallbackDeliveryMode::WakeHint,
        token: None,
        token_hash: "fixture-hash".into(),
        status: ExternalTriggerStatus::Active,
        created_at: delivered_at,
        revoked_at: None,
        last_delivered_at: Some(delivered_at),
        delivery_count: 1,
    };
    let mut waits = Vec::new();
    let mut frames = Vec::new();
    for (prefix, count, state) in [
        ("q", 9, WorkItemState::Open),
        ("y", 7, WorkItemState::Open),
        ("o", 5, WorkItemState::Open),
        ("b", 5, WorkItemState::Open),
        ("f", 5, WorkItemState::Completing),
        ("c", 5, WorkItemState::Completed),
        ("t", 1, WorkItemState::Open),
        ("z", 5, WorkItemState::Open),
    ] {
        for index in 0..count {
            let mut item = record(&format!("{prefix}{index:02}"), state.clone());
            match prefix {
                "y" => frames.push(WorkItemContinuationFrame::new_on_completed(
                    "web", &item.id, "current", None,
                )),
                "o" => item.plan_status = WorkItemPlanStatus::NeedsInput,
                "b" => item.blocked_by = Some("manual".into()),
                "t" => waits.push(wait(&item, WaitConditionKind::Task)),
                "z" => {
                    item.blocked_by = Some("triggered".into());
                    let mut condition = wait(&item, WaitConditionKind::External);
                    condition.wake_sources = vec![WakeSource::ExternalIngress {
                        external_trigger_id: Some("trigger-fixture".into()),
                    }];
                    waits.push(condition);
                }
                _ => {}
            }
            records.push(item);
        }
    }
    let allocations = records
        .iter()
        .map(|item| (item.id.clone(), ownership(item)))
        .collect::<HashMap<_, _>>();
    let expected = records
        .iter()
        .map(|item| {
            let projection = derive_work_item_scheduling(WorkItemSchedulingFacts {
                work_item: item,
                is_current: item.id == "current",
                is_yielded: item.id.starts_with('y'),
                active_wait_conditions: &waits,
                trigger_delivery_by_id: &deliveries,
            });
            (item.id.clone(), projection)
        })
        .collect::<HashMap<_, _>>();
    let mut expected_display = records
        .iter()
        .filter(|item| {
            item.state == WorkItemState::Open
                && item.id != "current"
                && !item.id.starts_with('y')
                && !item.id.starts_with('t')
        })
        .map(|item| (item.blocked_by.is_some(), item.id.clone()))
        .collect::<Vec<_>>();
    expected_display.sort();
    let queue = derive_context_work_queue(
        Some("current".into()),
        records
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect(),
        vec![trigger],
        waits,
        frames,
    )
    .unwrap();
    assert_eq!(queue.items.len(), expected.len());
    for projection in &queue.items {
        assert_eq!(projection, &expected[&projection.work_item.id]);
        assert_eq!(
            ownership(&projection.work_item),
            allocations[&projection.work_item.id]
        );
    }
    assert_eq!(queue.current.as_ref().unwrap().id, "current");
    assert_eq!(
        queue.current_runnable.as_ref().unwrap().work_item.id,
        "current"
    );
    let current = queue
        .items
        .iter()
        .find(|item| item.work_item.id == "current")
        .unwrap();
    assert_ne!(
        queue.current.as_ref().unwrap().id.as_ptr(),
        current.work_item.id.as_ptr()
    );
    assert_ne!(
        queue
            .current_runnable
            .as_ref()
            .unwrap()
            .work_item
            .id
            .as_ptr(),
        current.work_item.id.as_ptr()
    );
    assert_eq!(
        ids(&queue.queued_runnable),
        ["q00", "q01", "q02", "q03", "q04"]
    );
    assert_eq!(ids(&queue.yielded), ["y00", "y01", "y02", "y03", "y04"]);
    assert_eq!(ids(&queue.waiting_for_operator), ["o00", "o01", "o02"]);
    assert_eq!(ids(&queue.triggered_blocked), ["z00", "z01", "z02"]);
    assert_eq!(ids(&queue.blocked), ["b00", "b01", "b02"]);
    assert_eq!(ids(&queue.completing), ["f00", "f01", "f02"]);
    assert_eq!(ids(&queue.completed_recent), ["c00", "c01", "c02"]);
    assert_eq!(
        queue
            .queued_blocked
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        expected_display
            .iter()
            .map(|(_, id)| id.as_str())
            .collect::<Vec<_>>()
    );
    for item in &queue.queued_blocked {
        let canonical = queue
            .items
            .iter()
            .find(|projection| projection.work_item.id == item.id)
            .unwrap();
        assert_eq!(item, &canonical.work_item);
        assert_ne!(
            item.objective.as_ptr(),
            canonical.work_item.objective.as_ptr()
        );
    }
    assert_eq!(
        ids(&queue.items),
        [
            "current", "z00", "z01", "z02", "z03", "z04", "q00", "q01", "q02", "q03", "q04", "q05",
            "q06", "q07", "q08", "y00", "y01", "y02", "y03", "y04", "y05", "y06", "o00", "o01",
            "o02", "o03", "o04", "b00", "b01", "b02", "b03", "b04", "t00", "f00", "f01", "f02",
            "f03", "f04", "c00", "c01", "c02", "c03", "c04",
        ]
    );
}

#[test]
fn context_queue_keeps_missing_terminal_and_empty_current_semantics() {
    for current in [None, Some("missing"), Some("closed"), Some("finishing")] {
        let queue = derive_context_work_queue(
            current,
            [
                record("closed", WorkItemState::Completed),
                record("finishing", WorkItemState::Completing),
                record("runnable", WorkItemState::Open),
            ]
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        assert!(queue.current.is_none());
        assert!(queue.current_runnable.is_none());
        assert_eq!(ids(&queue.queued_runnable), ["runnable"]);
        assert!(queue.items.iter().all(|item| !item.is_current));
    }
    let empty = derive_context_work_queue(
        Some("missing".into()),
        QueueLatestWork::new(),
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    assert!(empty.current.is_none());
    assert!(empty.current_runnable.is_none());
    assert!(empty.items.is_empty());
    assert!(empty.queued_blocked.is_empty());
}

#[test]
fn queue_record_moves_preserve_phase_and_items_slots_do_not_reset_failure() -> Result<()> {
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    for failed in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let scope = InputEvidenceScope {
            agent_id: "queue-record-move".into(),
            identity_incarnation: 1,
        };
        let header_bytes = (scope.agent_id.len() as u64 + 256) * 8;
        let header_records = 8 * 8;
        let slot_bytes = std::mem::size_of::<WorkItemSchedulingProjection>() as u64 * 8;
        let slot_records = 8;
        let prefix_bytes = header_bytes + 64;
        let prefix_records = header_records + 8;
        // The queue independently clones the selected TodoItem. Its two
        // string fields have nine structural cells, regardless of text.
        let selected = record("moved", WorkItemState::Open).todo_list.remove(1);
        let todo_bytes = (serde_json::to_vec(&selected)?.len() as u64 * 4 + 9 * 128 + 512) * 8;
        let todo_records = (9 + 16) * 8;
        let task_wait = wait(
            &record("moved", WorkItemState::Open),
            WaitConditionKind::Task,
        );
        let summary =
            super::queue_wait_summary_material_tests::cost(std::slice::from_ref(&task_wait))?;
        let total_bytes = prefix_bytes + slot_bytes + todo_bytes + summary.0;
        let total_records = prefix_records + slot_records + todo_records + summary.1;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope, total_bytes, total_records)?;
        assert_eq!(phase.finish()?, (header_bytes, header_records));
        assert_eq!(ledger.report()?.observed_bytes, header_bytes);
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            resource_io::charge_owned_metadata(8, 1)?;
            assert_eq!(phase.finish()?, (prefix_bytes, prefix_records));
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &holder
            ));
            if failed {
                assert!(resource_io::charge_owned_metadata(
                    (slot_bytes + todo_bytes + summary.0) / 8 + 1,
                    0,
                )
                .is_err());
            }
            let original = record("moved", WorkItemState::Open);
            let allocation = ownership(&original);
            // Record movement itself remains uncharged even after failure.
            // The queue's separate slots and selected Todo must be admitted.
            let task_wait = wait(&original, WaitConditionKind::Task);
            let projection = derive_owned_work_item_scheduling(
                original,
                false,
                false,
                std::slice::from_ref(&task_wait),
                &|_| None,
            );
            assert_eq!(ownership(&projection.work_item), allocation);
            assert_eq!(
                projection.scheduling_state,
                WorkItemSchedulingState::WaitingTask
            );
            assert_eq!(ledger.report()?.observed_bytes, prefix_bytes);
            let result = derive_context_work_queue(
                None,
                QueueLatestWork::from([(projection.work_item.id.clone(), projection.work_item)]),
                vec![],
                vec![task_wait],
                vec![],
            );
            if failed {
                let queue_error = result.unwrap_err();
                assert!(queue_error
                    .to_string()
                    .contains("queue items slots admission"));
                assert!(queue_error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix_bytes);
                let error = phase.finish().unwrap_err();
                assert_eq!(
                    error
                        .downcast_ref::<crate::runtime_error::RuntimeError>()
                        .unwrap()
                        .descriptor()
                        .code,
                    "context_resource_exhausted"
                );
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(resource_io::charge_owned_metadata(0, 0).is_err());
            } else {
                let queue = result?;
                assert_eq!(ownership(&queue.items[0].work_item), allocation);
                assert_eq!(
                    queue.items[0].scheduling_state,
                    WorkItemSchedulingState::WaitingTask
                );
                assert!(queue.queued_blocked.is_empty());
                assert!(queue.blocked.is_empty());
                assert_eq!(ledger.report()?.observed_bytes, total_bytes);
                assert_eq!(phase.finish()?, (total_bytes, total_records));
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &holder
                ));
            }
        }
        drop(phase);
        assert_eq!(
            ledger.report()?.observed_bytes,
            if failed { prefix_bytes } else { total_bytes }
        );
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let plain = derive_context_work_queue(
        None,
        QueueLatestWork::from([("plain".into(), record("plain", WorkItemState::Open))]),
        vec![],
        vec![],
        vec![],
    )?;
    assert_eq!(ids(&plain.queued_runnable), ["plain"]);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
