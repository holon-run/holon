use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    types::{
        CallbackDeliveryMode, ExternalTriggerScope, ExternalTriggerStatus, TodoItem, TodoItemState,
        WaitConditionKind, WakeSource, WorkItemPlanStatus,
    },
    work_item_scheduling::ContextWorkQueue,
};

fn capture(records: Vec<CanonicalWorkRecord>) -> Result<CapturedWork> {
    let records = records
        .into_iter()
        .map(|record| {
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

fn work(index: usize, state: WorkItemState) -> WorkItemRecord {
    let CanonicalWorkRecord::Work(mut work) = qualified(&format!("context-{index:03}"), state, 1)
        .record
        .clone()
    else {
        unreachable!()
    };
    work.plan_status = WorkItemPlanStatus::Ready;
    work.updated_at += chrono::Duration::seconds(index as i64);
    work
}

fn wait(work: &WorkItemRecord, kind: WaitConditionKind) -> WaitConditionRecord {
    WaitConditionRecord {
        id: format!("wait-{}", work.id),
        agent_id: scope().agent_id,
        work_item_id: Some(work.id.clone()),
        status: WaitConditionStatus::Active,
        kind,
        source: Some("context fixture".into()),
        subject_ref: None,
        waiting_for: "context fixture".into(),
        wake_sources: vec![WakeSource::ExternalIngress {
            external_trigger_id: Some("context-trigger".into()),
        }],
        continuation: None,
        created_at: work.created_at,
        updated_at: work.updated_at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    }
}

#[test]
fn context_selection_matches_full_queue_for_all_candidate_classes() -> Result<()> {
    let mut records = Vec::new();
    for index in 0..80 {
        let mut item = work(
            index,
            match index % 8 {
                6 => WorkItemState::Completing,
                7 => WorkItemState::Completed,
                _ => WorkItemState::Open,
            },
        );
        item.todo_list = vec![TodoItem {
            text: format!("todo {}", item.id),
            state: TodoItemState::Pending,
        }];
        match index % 8 {
            1 => {
                item.blocked_by = Some("external".into());
                records.push(CanonicalWorkRecord::Wait(wait(
                    &item,
                    WaitConditionKind::External,
                )));
            }
            2 => {
                records.push(CanonicalWorkRecord::Frame(
                    WorkItemContinuationFrame::new_on_completed(
                        scope().agent_id,
                        &item.id,
                        "context-000",
                        None,
                    ),
                ));
            }
            3 => item.plan_status = WorkItemPlanStatus::NeedsInput,
            4 => item.blocked_by = Some("manual".into()),
            5 => records.push(CanonicalWorkRecord::Wait(wait(
                &item,
                WaitConditionKind::Task,
            ))),
            _ => {}
        }
        records.push(CanonicalWorkRecord::Work(item));
    }
    let captured = capture(records)?;
    let mut state = AgentState::new(scope().agent_id);
    let at = chrono::DateTime::from_timestamp_millis(2_000).unwrap();
    let mut trigger = ExternalTriggerRecord {
        external_trigger_id: "context-trigger".into(),
        target_agent_id: state.id.clone(),
        scope: ExternalTriggerScope::Agent,
        delivery_mode: CallbackDeliveryMode::WakeHint,
        token: None,
        token_hash: "fixture-only".into(),
        status: ExternalTriggerStatus::Active,
        created_at: at,
        revoked_at: None,
        last_delivered_at: Some(at),
        delivery_count: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    // Include a blocked focus, task wait focus, completed focus and no focus;
    // trigger revocation must also preserve the full queue's classification.
    for focused in [None, Some(0), Some(1), Some(5), Some(7)] {
        state.current_work_item_id = focused.map(|index| format!("context-{index:03}"));
        for status in [
            ExternalTriggerStatus::Active,
            ExternalTriggerStatus::Revoked,
        ] {
            trigger.status = status;
            let triggers = [trigger.clone()];
            let full = captured.work_queue(&state, triggers.to_vec())?;
            let expected = ContextWorkQueue::from(full.clone());
            let phase = ResourceReadMaterial::new(&ledger, &scope())?;
            let actual = captured.context_work_queue_with_material(&state, &triggers, &phase)?;
            assert_eq!(actual, expected);
            phase.finish()?;
            let waits = captured.queue_active_waits().cloned().collect::<Vec<_>>();
            for has_queued_input in [false, true] {
                let expected_posture =
                    crate::storage::derive_agent_posture(&state, has_queued_input, &full, &waits)?;
                let observations = || crate::runtime::scheduler::SchedulerObservations {
                    active_tasks: vec![],
                    has_interrupted_replay: false,
                    active_wait_conditions: &waits,
                    activation_waits: vec![],
                    execution_snapshot: None,
                    active_timers: 0,
                    runtime_error: false,
                };
                let expected_projection =
                    crate::runtime::scheduler::SchedulerProjection::from_borrowed_observations(
                        crate::runtime::scheduler::SchedulerAgentSnapshot::from_state(&state),
                        usize::from(has_queued_input),
                        &full,
                        observations(),
                        at,
                    );
                let phase = ResourceReadMaterial::new(&ledger, &scope())?;
                phase.read(|| {
                    let ranked = captured.queue_work_ranks(&state, &triggers)?;
                    for rank in &ranked {
                        let expected = full
                            .items
                            .iter()
                            .find(|item| item.work_item.id == rank.work_item.id)
                            .expect("ranked Work must belong to the full queue");
                        assert_eq!(rank.has_active_waits, expected.has_active_waits);
                    }
                    let selected = captured.scheduler_work_selection(&state, &ranked)?;
                    let posture = captured.scheduler_work_posture(
                        &state,
                        has_queued_input,
                        &ranked,
                        &waits,
                    )?;
                    assert_eq!(posture, expected_posture);
                    let projection =
                        crate::runtime::scheduler::SchedulerProjection::from_selected_observations(
                            crate::runtime::scheduler::SchedulerAgentSnapshot::from_state(&state),
                            usize::from(has_queued_input),
                            selected,
                            observations(),
                            at,
                        );
                    assert_eq!(projection, expected_projection);
                    assert_eq!(
                        crate::runtime::scheduler::scheduling_advisories_for_selected_facts(
                            &state,
                            &projection,
                            &posture,
                            captured.scheduler_blocked_work(&ranked),
                            &waits,
                        ),
                        crate::runtime::scheduler::scheduling_advisories_for_facts(
                            &state,
                            &projection,
                            &posture,
                            &full,
                            &waits,
                        ),
                    );
                    Ok(())
                })?;
            }
        }
    }
    Ok(())
}

#[test]
fn scheduler_borrows_full_inventory_and_preserves_exact_claim_outside_candidate_window(
) -> Result<()> {
    let captured = capture(
        (0..200)
            .map(|index| {
                let mut item = work(index, WorkItemState::Open);
                item.objective = format!("scheduler {index} 汉🦀").repeat(128);
                item.todo_list = (0..8)
                    .map(|todo| TodoItem {
                        text: format!("todo {index}/{todo}"),
                        state: TodoItemState::Pending,
                    })
                    .collect();
                CanonicalWorkRecord::Work(item)
            })
            .collect(),
    )?;
    let state = AgentState::new(scope().agent_id);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let baseline = ledger.report()?.observed_bytes;
    let read = |phase: &ResourceReadMaterial| {
        phase.read(|| {
            let ranked = captured.queue_work_ranks(&state, &[])?;
            assert_eq!(ranked.len(), 200);
            let selected = captured.scheduler_work_selection(&state, &ranked)?;
            assert_eq!(selected.queued_runnable.len(), 5);
            crate::diagnostics::resource_io::admit_cloned_value(&selected)?;
            let retained = selected.clone();
            let exact = captured
                .scheduler_work_projection(&state, &[], "context-199")?
                .unwrap();
            assert_eq!(
                exact.work_item,
                *ranked
                    .iter()
                    .find(|rank| rank.work_item.id == "context-199")
                    .unwrap()
                    .work_item
            );
            assert!(selected
                .queued_runnable
                .iter()
                .all(|work| work.id != exact.id));
            assert_eq!(selected, retained);
            assert!(captured
                .scheduler_work_projection(&state, &[], "missing")?
                .is_none());
            Ok(())
        })
    };
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    read(&phase)?;
    let (bytes, records) = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
    read(&phase)?;
    phase.finish()?;
    drop(phase);
    for (bytes, records) in [(bytes - 1, records), (bytes, records - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
        let error = read(&phase).unwrap_err();
        assert_eq!(
            crate::runtime_error::describe_runtime_error(&error).code,
            "context_resource_exhausted"
        );
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let error = captured
        .work_queue_with_material(&state, &[], &phase)
        .unwrap_err();
    assert_eq!(
        crate::runtime_error::describe_runtime_error(&error).code,
        "context_resource_exhausted"
    );
    assert!(phase.finish().is_err());
    Ok(())
}

#[test]
fn context_borrows_complete_inventory_and_owns_only_original_display_window() -> Result<()> {
    let captured = capture(
        (0..200)
            .map(|index| {
                let mut item = work(index, WorkItemState::Open);
                if index >= 5 {
                    item.objective = "unselected body 汉字🦀".repeat(128);
                }
                CanonicalWorkRecord::Work(item)
            })
            .collect(),
    )?;
    let state = AgentState::new(scope().agent_id);
    let expected = ContextWorkQueue::from(captured.work_queue(&state, vec![])?);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let baseline = ledger.report()?.observed_bytes;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let actual = captured.context_work_queue_with_material(&state, &[], &phase)?;
    assert_eq!(actual, expected);
    assert_eq!(actual.queued_runnable.len(), 5);
    let (bytes, records) = phase.finish()?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    // The exact admitted phase succeeds; either short axis still latches.
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
    assert_eq!(
        captured.context_work_queue_with_material(&state, &[], &phase)?,
        expected
    );
    phase.finish()?;
    drop(phase);
    for (bytes, records) in [(bytes - 1, records), (bytes, records - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
        let error = captured
            .context_work_queue_with_material(&state, &[], &phase)
            .unwrap_err();
        assert_eq!(
            crate::runtime_error::describe_runtime_error(&error).code,
            "context_resource_exhausted"
        );
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    Ok(())
}
