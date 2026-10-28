use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    types::{TodoItem, TodoItemState, WorkItemPlanStatus},
};

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn key_cost(id: &str) -> (u64, usize) {
    ((std::mem::size_of::<String>() + id.len()) as u64 * 8, 8)
}

fn clone_cost(work: &WorkItemRecord) -> Result<(u64, usize)> {
    // Count the decoded shape, independently of the streaming JSON scanner.
    fn cells(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(_) => 1,
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(cells).sum::<usize>()
            }
            serde_json::Value::Object(values) => {
                2 + 2 * values.len()
                    + values.len().saturating_sub(1)
                    + values.values().map(cells).sum::<usize>()
            }
            _ => 0,
        }
    }
    let count = cells(&serde_json::to_value(work)?);
    Ok((
        (serde_json::to_vec(work)?.len() as u64 * 4 + count as u64 * 128 + 512) * 8,
        (count + 16) * 8,
    ))
}

fn work(id: &str, state: WorkItemState, text: &str) -> WorkItemRecord {
    let evidence = qualified(id, state, 1);
    let CanonicalWorkRecord::Work(mut work) = evidence.record.clone() else {
        unreachable!()
    };
    work.plan_status = WorkItemPlanStatus::Ready;
    work.objective = text.into();
    work.todo_list = vec![
        TodoItem {
            text: format!("pending: {text}"),
            state: TodoItemState::Pending,
        },
        TodoItem {
            text: format!("done: {text}"),
            state: TodoItemState::Completed,
        },
    ];
    work.result_summary = Some(format!("summary: {text}"));
    work
}

fn capture(works: &[WorkItemRecord]) -> Result<CapturedWork> {
    let records = works
        .iter()
        .map(|work| {
            let record = CanonicalWorkRecord::Work(work.clone());
            Arc::new(QualifiedWorkRecord {
                scope: scope(),
                key: record.key(),
                revision: 1,
                content_digest: "a".repeat(64),
                resident_bytes: serde_json::to_vec(&record).unwrap().len(),
                record,
            })
        })
        .collect::<Vec<_>>();
    domain(records.clone(), coverage(&records, 1)).capture(&WorkCaptureRequest::current())
}

#[test]
fn resident_work_clone_exact_shape_cost_and_independent_nested_ownership() -> Result<()> {
    for text in ["", "汉字🦀\0\n\t\"{}[],:\\", &"long中".repeat(4096)] {
        let original = work("resident-中🦀", WorkItemState::Open, text);
        assert_eq!(clone_queue_work_record(&original)?, original);
        let total = add(header(), clone_cost(&original)?);
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
        let holder = phase.guard()?;
        let mut copy = {
            let _entry = phase.enter();
            let copy = clone_queue_work_record(&original)?;
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            copy
        };
        assert_eq!(copy, original);
        assert_ne!(copy.id.as_ptr(), original.id.as_ptr());
        assert_ne!(copy.todo_list.as_ptr(), original.todo_list.as_ptr());
        assert_ne!(
            copy.todo_list[0].text.as_ptr(),
            original.todo_list[0].text.as_ptr()
        );
        copy.todo_list[0].text.push_str(" changed");
        assert_ne!(copy.todo_list[0].text, original.todo_list[0].text);
        assert_eq!(phase.finish()?, total);
        drop((phase, copy));
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn resident_work_clone_rejects_each_axis_after_success_and_latches() -> Result<()> {
    let original = work("work", WorkItemState::Open, "中\0\"\\{}[],:");
    let prefix = add(header(), clone_cost(&original)?);
    let full = add(prefix, clone_cost(&original)?);
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            full.0 - u64::from(bytes),
            full.1 - usize::from(!bytes),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let first = clone_queue_work_record(&original)?;
            for _ in 0..2 {
                let error = clone_queue_work_record(&original).unwrap_err();
                assert_eq!(error.to_string(), "resident queue WorkItem clone admission");
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(first, original);
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, prefix.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert_eq!(clone_queue_work_record(&original)?, original);
    Ok(())
}

#[test]
fn resident_queue_caller_rejects_record_after_key_without_partial_result() -> Result<()> {
    let original = work("work-中🦀", WorkItemState::Open, "escaped\0\"\\[]{}");
    let captured = capture(std::slice::from_ref(&original))?;
    let state = AgentState::new(scope().agent_id);
    let displayed = (std::mem::size_of::<&str>() as u64 * 8, 8);
    let prefix = add(add(header(), displayed), key_cost(&original.id));
    let full = add(prefix, clone_cost(&original)?);
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            full.0 - u64::from(bytes),
            full.1 - usize::from(!bytes),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            for attempt in 0..2 {
                let error = captured.work_queue(&state, vec![]).unwrap_err();
                assert_eq!(
                    error.to_string(),
                    if attempt == 0 {
                        "resident queue WorkItem clone admission"
                    } else {
                        "resident displayed-ID slots admission"
                    }
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        assert!(matches!(
            &captured.records[0].record,
            CanonicalWorkRecord::Work(record) if record == &original
        ));
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, prefix.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert_eq!(captured.work_queue(&state, vec![])?.items.len(), 1);
    Ok(())
}

#[test]
fn resident_queue_caller_rejects_latest_slots_after_record_on_original_phase() -> Result<()> {
    let original = work("work-中🦀", WorkItemState::Open, "escaped\0\"\\[]{}");
    let captured = capture(std::slice::from_ref(&original))?;
    let state = AgentState::new(scope().agent_id);
    let displayed = (std::mem::size_of::<&str>() as u64 * 8, 8);
    let prefix = add(
        add(add(header(), displayed), key_cost(&original.id)),
        clone_cost(&original)?,
    );
    let full = add(
        prefix,
        (
            (4 * std::mem::size_of::<(String, WorkItemRecord, usize)>()) as u64 * 8,
            32,
        ),
    );
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            full.0 - u64::from(bytes),
            full.1 - usize::from(!bytes),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            for attempt in 0..2 {
                let error = captured.work_queue(&state, vec![]).unwrap_err();
                assert_eq!(
                    error.to_string(),
                    if attempt == 0 {
                        "queue latest slots admission"
                    } else {
                        "resident displayed-ID slots admission"
                    }
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        assert!(matches!(&captured.records[0].record,
            CanonicalWorkRecord::Work(record) if record == &original));
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, prefix.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert_eq!(captured.work_queue(&state, vec![])?.items.len(), 1);
    Ok(())
}

fn wait(id: &str, owner: Option<&str>) -> WaitConditionRecord {
    let at = chrono::DateTime::from_timestamp_millis(1_000).unwrap();
    WaitConditionRecord {
        id: id.into(),
        agent_id: scope().agent_id,
        work_item_id: owner.map(str::to_owned),
        status: WaitConditionStatus::Active,
        kind: crate::types::WaitConditionKind::Task,
        source: None,
        subject_ref: Some("task".into()),
        waiting_for: format!("wait {id}"),
        wake_sources: vec![crate::types::WakeSource::TaskResult {
            task_id: "task".into(),
        }],
        continuation: None,
        created_at: at,
        updated_at: at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    }
}

#[test]
fn resident_wait_owner_filter_borrows_final_map_and_retains_original_vec() -> Result<()> {
    let works = [
        work("", WorkItemState::Open, ""),
        work("open", WorkItemState::Completed, "replaced"),
        work("done", WorkItemState::Completed, "done"),
        work("completing", WorkItemState::Completing, "completing"),
        work("open", WorkItemState::Open, "last wins"),
    ];
    let map = works
        .iter()
        .map(|work| (work.id.clone(), work.clone()))
        .collect();
    let mut waits = vec![
        wait("global", None),
        wait("missing", Some("missing")),
        wait("open-1", Some("open")),
        wait("completing", Some("completing")),
        wait("empty-owner", Some("")),
        wait("done", Some("done")),
        wait("open-2", Some("open")),
        wait("global-2", None),
    ];
    let expected = crate::storage::filter_context_active_waits(waits.clone(), works.to_vec());
    let buffer = waits.as_ptr();
    let capacity = waits.capacity();
    let text = waits[2].waiting_for.as_ptr();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    {
        let _entry = phase.enter();
        retain_queue_active_waits(&mut waits, &map);
        assert_eq!(phase.finish()?, header());
    }
    assert_eq!(waits, expected);
    assert_eq!(
        waits
            .iter()
            .map(|wait| wait.id.as_str())
            .collect::<Vec<_>>(),
        ["global", "open-1", "empty-owner", "open-2", "global-2"]
    );
    assert_eq!(waits.as_ptr(), buffer);
    assert_eq!(waits.capacity(), capacity);
    assert_eq!(waits[1].waiting_for.as_ptr(), text);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut empty = Vec::new();
    retain_queue_active_waits(&mut empty, &map);
    assert_eq!(empty.capacity(), 0);
    Ok(())
}

#[test]
fn resident_queue_success_moves_admitted_records_through_consumption_and_last_holder() -> Result<()>
{
    let works = [
        work("open-中", WorkItemState::Open, "nested \"{}[]\0🦀"),
        work("done-🦀", WorkItemState::Completed, "history"),
    ];
    let captured = capture(&works)?;
    let state = AgentState::new(scope().agent_id);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let baseline = ResourceReadMaterial::new(&ledger, &scope())?;
    let expected = {
        let _entry = baseline.enter();
        crate::storage::derive_context_work_queue(
            None,
            works
                .iter()
                .map(|work| (work.id.clone(), work.clone()))
                .collect(),
            vec![],
            vec![],
            vec![],
        )?
    };
    let mut total = baseline.finish()?;
    drop(baseline);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for work in &works {
        total = add(add(total, key_cost(&work.id)), clone_cost(work)?);
    }
    total = add(
        total,
        (
            (4 * std::mem::size_of::<(String, WorkItemRecord, usize)>()) as u64 * 8,
            32,
        ),
    );
    total = add(
        total,
        (
            (works.len() * std::mem::size_of::<&str>()) as u64 * 8,
            works.len() * 8,
        ),
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let holder = phase.guard()?;
    let queue = {
        let _entry = phase.enter();
        let queue = captured.work_queue(&state, vec![])?;
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        queue
    };
    assert_eq!(queue.items, expected.items);
    assert_eq!(queue.current, expected.current);
    assert_eq!(queue.completed_recent, expected.completed_recent);
    assert_eq!(phase.finish()?, total);
    drop((phase, captured));
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    let json = serde_json::to_vec(&queue.items)?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&json)?,
        serde_json::to_value(&expected.items)?
    );
    struct Fail;
    impl std::io::Write for Fail {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected consumer failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(serde_json::to_writer(Fail, &queue.items).is_err());
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop((queue, json, expected));
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
