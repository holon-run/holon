use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    types::{WaitConditionKind, WakeSource},
};

mod trigger_clone_tests;

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn slots<T>(count: usize) -> (u64, usize) {
    ((count * std::mem::size_of::<T>()) as u64 * 8, count * 8)
}

fn clone_cost(record: &impl Serialize) -> Result<(u64, usize)> {
    // Independent tree oracle, not the production streaming shape counter.
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
    let count = cells(&serde_json::to_value(record)?);
    Ok((
        (serde_json::to_vec(record)?.len() as u64 * 4 + count as u64 * 128 + 512) * 8,
        (count + 16) * 8,
    ))
}

fn wait(id: &str, text: &str) -> WaitConditionRecord {
    let at = chrono::DateTime::from_timestamp_millis(1_000).unwrap();
    WaitConditionRecord {
        id: id.into(),
        agent_id: scope().agent_id,
        work_item_id: Some("open".into()),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Task,
        source: Some(text.into()),
        subject_ref: Some(text.into()),
        waiting_for: text.into(),
        wake_sources: vec![WakeSource::TaskResult {
            task_id: text.into(),
        }],
        continuation: Some(serde_json::json!({"nested": [text, {"text": text}]})),
        created_at: at,
        updated_at: at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: Some(text.into()),
        trigger_message_id: None,
        triggered_at: None,
    }
}

fn frame(text: &str) -> WorkItemContinuationFrame {
    let mut frame =
        WorkItemContinuationFrame::new_on_completed(scope().agent_id, "open", "active", None);
    frame.id = "frame".into();
    frame.turn_id = Some(text.into());
    frame.resolution_reason = Some(text.into());
    frame.created_at = chrono::DateTime::from_timestamp_millis(1_000).unwrap();
    frame.updated_at = frame.created_at;
    frame
}

fn copies<T: Serialize + Clone + PartialEq + std::fmt::Debug>(
    record: &T,
    count: usize,
) -> Result<Vec<T>> {
    let capacity = if count == 0 {
        0
    } else {
        count.next_power_of_two().max(4)
    };
    let clone = clone_cost(record)?;
    let total = add(
        add(header(), slots::<T>(capacity)),
        (clone.0 * count as u64, clone.1 * count),
    );
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let holder = phase.guard()?;
    let mut copies = Vec::new();
    {
        let _entry = phase.enter();
        for _ in 0..count {
            push_queue_record_clone(&mut copies, record)?;
        }
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
    }
    assert_eq!(copies.len(), count);
    assert_eq!(copies.capacity(), capacity);
    assert!(copies.iter().all(|copy| copy == record));
    assert_eq!(phase.finish()?, total);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(copies)
}

#[test]
fn resident_wait_vec_geometric_slots_and_typed_clone_cost_are_exact() -> Result<()> {
    for text in ["", "汉字🦀\0\n\t\"{}[],:\\", &"long中".repeat(2048)] {
        let original = wait("wait", text);
        for count in [0, 1, 4, 5, 17] {
            let mut copied = copies(&original, count)?;
            if !text.is_empty() && count > 0 {
                assert_ne!(
                    copied[0].waiting_for.as_ptr(),
                    original.waiting_for.as_ptr()
                );
                assert_ne!(
                    copied[0].wake_sources.as_ptr(),
                    original.wake_sources.as_ptr()
                );
                copied[0].continuation.as_mut().unwrap()["nested"][1]["text"] =
                    serde_json::json!("changed");
                assert_ne!(copied[0].continuation, original.continuation);
            }
        }
    }
    Ok(())
}

#[test]
fn resident_frame_vec_geometric_slots_and_typed_clone_cost_are_exact() -> Result<()> {
    for text in ["", "汉字🦀\0\n\t\"{}[],:\\", &"long中".repeat(2048)] {
        let original = frame(text);
        for count in [0, 1, 4, 5, 17] {
            let mut copied = copies(&original, count)?;
            if count > 0 {
                assert_ne!(copied[0].id.as_ptr(), original.id.as_ptr());
                assert_ne!(
                    copied[0].suspended_work_item_id.as_ptr(),
                    original.suspended_work_item_id.as_ptr()
                );
                copied[0].turn_id.as_mut().unwrap().push_str(" changed");
                assert_ne!(copied[0].turn_id, original.turn_id);
            }
        }
    }
    Ok(())
}

fn reject_gates<T: Serialize + Clone + PartialEq + std::fmt::Debug>(record: &T) -> Result<()> {
    let clone = clone_cost(record)?;
    let prefix = add(add(header(), slots::<T>(4)), (clone.0 * 4, clone.1 * 4));
    for (name, before, gate) in [
        (
            "resident queue record slots admission",
            prefix,
            slots::<T>(4),
        ),
        (
            "resident queue record clone admission",
            add(prefix, slots::<T>(4)),
            clone,
        ),
    ] {
        let full = add(before, gate);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                full.0 - u64::from(bytes),
                full.1 - usize::from(!bytes),
            )?;
            let holder = phase.guard()?;
            let mut records = Vec::new();
            {
                let _entry = phase.enter();
                for _ in 0..4 {
                    push_queue_record_clone(&mut records, record)?;
                }
                for _ in 0..2 {
                    let error = push_queue_record_clone(&mut records, record).unwrap_err();
                    assert_eq!(error.to_string(), name);
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(records.len(), 4);
                    assert!(records.iter().all(|copy| copy == record));
                    assert_eq!(
                        records.capacity(),
                        if name.contains("slots") { 4 } else { 8 }
                    );
                    assert_eq!(ledger.report()?.observed_bytes, before.0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
            }
            drop((records, phase));
            assert_eq!(ledger.report()?.observed_bytes, before.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn resident_record_growth_and_clone_reject_each_axis_after_success_and_latch() -> Result<()> {
    reject_gates(&wait("wait", "nested中\0[]{}\""))?;
    reject_gates(&frame("frame中\0[]{}\""))
}

fn capture() -> Result<CapturedWork> {
    let mut triggered = wait("triggered", "excluded");
    triggered.status = WaitConditionStatus::Triggered;
    let mut global = wait("global", "global中\0\"\\[]{}");
    global.work_item_id = None;
    let mut resumed = frame("excluded");
    resumed.id = "resumed".into();
    resumed.state = WorkItemContinuationState::Resumed;
    let mut records = vec![
        qualified("open", WorkItemState::Open, 1),
        qualified("active", WorkItemState::Open, 1),
    ];
    for record in [
        CanonicalWorkRecord::Wait(wait("wait", "selected中\0\"[]{}")),
        CanonicalWorkRecord::Wait(global),
        CanonicalWorkRecord::Wait(triggered),
        CanonicalWorkRecord::Frame(frame("selected中\0\"[]{}")),
        CanonicalWorkRecord::Frame(resumed),
    ] {
        records.push(Arc::new(QualifiedWorkRecord {
            scope: scope(),
            key: record.key(),
            revision: 1,
            content_digest: "a".repeat(64),
            resident_bytes: serde_json::to_vec(&record)?.len(),
            record,
        }));
    }
    let mut coverage = coverage(&records, 1);
    coverage
        .mandatory_roots
        .extend(records.iter().map(|record| record.key.clone()));
    domain(records, coverage).capture(&WorkCaptureRequest::current())
}

fn selected_wait(captured: &CapturedWork, wait: &WaitConditionRecord) -> bool {
    captured.unresolved_wait_ids.contains(&wait.id) && wait.status == WaitConditionStatus::Active
}

fn steps(captured: &CapturedWork) -> Result<Vec<(&'static str, (u64, usize))>> {
    let count = captured
        .records
        .iter()
        .filter(|record| {
            matches!(
                &record.record, CanonicalWorkRecord::Work(work) if work.state == WorkItemState::Open
            )
        })
        .count()
        + captured.nonopen.keys.len();
    let mut steps = vec![("displayed", slots::<&str>(count))];
    let (mut waits, mut frames, mut works) = (0usize, 0usize, 0usize);
    for evidence in &captured.records {
        match &evidence.record {
            CanonicalWorkRecord::Work(work) => {
                steps.push((
                    "key",
                    (
                        (std::mem::size_of::<String>() + work.id.len()) as u64 * 8,
                        8,
                    ),
                ));
                steps.push(("work", clone_cost(work)?));
                if works == 0 || (works >= 4 && works.is_power_of_two()) {
                    steps.push((
                        "queue latest slots admission",
                        slots::<(String, WorkItemRecord, usize)>(works.max(4)),
                    ));
                }
                works += 1;
            }
            CanonicalWorkRecord::Wait(wait) if selected_wait(captured, wait) => {
                if waits == 0 || (waits >= 4 && waits.is_power_of_two()) {
                    steps.push((
                        "resident queue record slots admission",
                        slots::<WaitConditionRecord>(waits.max(4)),
                    ));
                }
                steps.push(("resident queue record clone admission", clone_cost(wait)?));
                waits += 1;
            }
            CanonicalWorkRecord::Frame(frame) if captured.active_frame_ids.contains(&frame.id) => {
                if frames == 0 || (frames >= 4 && frames.is_power_of_two()) {
                    steps.push((
                        "resident queue record slots admission",
                        slots::<WorkItemContinuationFrame>(frames.max(4)),
                    ));
                }
                steps.push(("resident queue record clone admission", clone_cost(frame)?));
                frames += 1;
            }
            _ => {}
        }
    }
    Ok(steps)
}

fn legacy_queue(
    captured: &CapturedWork,
) -> Result<(
    crate::work_item_scheduling::WorkQueueReadModel,
    (u64, usize),
)> {
    let mut works = Vec::new();
    let mut waits = Vec::new();
    let mut frames = Vec::new();
    // Old projection copies happen outside the measured suffix. Their costs
    // are independently enumerated by steps(), not inferred from new gates.
    for evidence in &captured.records {
        match &evidence.record {
            CanonicalWorkRecord::Work(work) => {
                works.push((work.id.clone(), work.clone()));
            }
            CanonicalWorkRecord::Wait(wait) if selected_wait(captured, wait) => {
                waits.push(wait.clone())
            }
            CanonicalWorkRecord::Frame(frame) if captured.active_frame_ids.contains(&frame.id) => {
                frames.push(frame.clone())
            }
            _ => {}
        }
    }
    let works = works.into_iter().collect();
    retain_queue_active_waits(&mut waits, &works);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let queue = {
        let _entry = phase.enter();
        crate::storage::derive_context_work_queue(None, works, vec![], waits, frames)?
    };
    Ok((queue, phase.finish()?))
}

#[test]
fn context_queue_adapter_preserves_membership_cost_consumption_and_last_holder() -> Result<()> {
    for excluded in [false, true] {
        let mut captured = capture()?;
        if excluded {
            captured.unresolved_wait_ids.clear();
            captured.active_frame_ids.clear();
        }
        let (expected, suffix) = legacy_queue(&captured)?;
        let total = steps(&captured)?
            .into_iter()
            .fold(suffix, |sum, (_, cost)| add(sum, cost));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
        let holder = phase.guard()?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let queue =
            captured.work_queue_with_material(&AgentState::new(scope().agent_id), &[], &phase)?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(queue.items, expected.items);
        assert_eq!(queue.queued_runnable, expected.queued_runnable);
        assert_eq!(queue.queued_blocked, expected.queued_blocked);
        assert_eq!(queue.current, expected.current);
        assert_eq!(phase.finish()?, total);
        assert!(Arc::ptr_eq(&holder, &phase.guard()?));
        drop((captured, phase));
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        assert_eq!(
            serde_json::to_value(&queue.items)?,
            serde_json::to_value(&expected.items)?
        );
        struct Fail;
        impl std::io::Write for Fail {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("consumer failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(serde_json::to_writer(Fail, &queue.items).is_err());
        drop(queue);
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn context_queue_adapter_propagates_every_record_gate_failure_without_partial_queue() -> Result<()>
{
    let captured = capture()?;
    let mut prefix = header();
    for (name, cost) in steps(&captured)? {
        let full = add(prefix, cost);
        if name.starts_with("resident queue record") {
            for bytes in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    full.0 - u64::from(bytes),
                    full.1 - usize::from(!bytes),
                )?;
                let holder = phase.guard()?;
                let state = AgentState::new(scope().agent_id);
                let error = captured
                    .work_queue_with_material(&state, &[], &phase)
                    .unwrap_err();
                assert_eq!(error.to_string(), name);
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                let retry = captured
                    .work_queue_with_material(&state, &[], &phase)
                    .unwrap_err();
                assert!(retry.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                drop(holder);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
        prefix = full;
    }
    Ok(())
}

#[test]
fn context_queue_adapter_restores_outer_material_without_reset_or_charge_migration() -> Result<()> {
    let captured = capture()?;
    let outer_ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let inner_ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let outer = ResourceReadMaterial::new(&outer_ledger, &scope())?;
    let inner = ResourceReadMaterial::new(&inner_ledger, &scope())?;
    let outer_holder = outer.guard()?;
    {
        let _entry = outer.enter();
        captured.work_queue_with_material(&AgentState::new(scope().agent_id), &[], &inner)?;
        assert!(Arc::ptr_eq(
            &outer_holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        assert_eq!(outer.finish()?, header());
        assert!(inner.finish()?.0 > header().0);
        let mut foreign = AgentState::new("foreign");
        foreign.current_work_item_id = Some("missing".into());
        assert_eq!(
            captured
                .work_queue_with_material(&foreign, &[], &inner)
                .unwrap_err()
                .to_string(),
            "Work projection State scope mismatch"
        );
        assert!(Arc::ptr_eq(
            &outer_holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        assert_eq!(outer.finish()?, header());
    }
    drop((inner, outer, outer_holder));
    assert_eq!(inner_ledger.report()?.observed_bytes, 0);
    assert_eq!(outer_ledger.report()?.observed_bytes, 0);
    Ok(())
}
