use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    work_item_scheduling::{derive_work_item_scheduling, WorkItemSchedulingFacts},
};
use chrono::Utc;
use std::sync::Arc;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "posture-material".into(),
        identity_incarnation: 1,
    }
}

fn item() -> super::super::work_queue::WorkItemSchedulingProjection {
    let mut record = WorkItemRecord::new("web", "posture fixture", WorkItemState::Open);
    record.id = "work_汉字\0🦀".repeat(23);
    derive_work_item_scheduling(WorkItemSchedulingFacts {
        work_item: &record,
        is_current: true,
        is_yielded: false,
        active_wait_conditions: &[],
        trigger_delivery_by_id: &BTreeMap::new(),
    })
}

// Independent oracle for the four final String allocations, not the selector.
fn cost(reason: &str, ids: [Option<&str>; 3]) -> (u64, usize) {
    let fields = std::iter::once(reason).chain(ids.into_iter().flatten());
    fields.fold((0, 0), |sum, text| {
        (
            sum.0 + (text.len() + std::mem::size_of::<String>()) as u64 * 8,
            sum.1 + 8,
        )
    })
}

#[test]
fn borrowed_posture_selection_preserves_precedence_reason_and_final_fields() -> Result<()> {
    let mut agent = AgentState::new("web");
    agent.status = AgentStatus::Stopped;
    agent.current_run_id = Some("run_🦀".into());
    agent.current_turn_work_item_id = Some("work_汉字".into());
    let stopped = derive_agent_posture_before_work_queue(&agent, true).unwrap();
    assert_eq!(stopped.into_owned().reason, "agent lifecycle is stopped");
    agent.status = AgentStatus::AwakeIdle;
    let active = derive_agent_posture_before_work_queue(&agent, true).unwrap();
    assert_eq!(
        active.work_item_id,
        agent.current_turn_work_item_id.as_deref()
    );
    assert_eq!(
        active.run_id.unwrap().as_ptr(),
        agent.current_run_id.as_ref().unwrap().as_ptr()
    );
    assert_eq!(active.into_owned().reason, "agent has an active turn");
    agent.current_run_id = None;
    assert_eq!(
        derive_agent_posture_before_work_queue(&agent, true)
            .unwrap()
            .into_owned()
            .reason,
        "agent has queued input"
    );
    assert!(derive_agent_posture_before_work_queue(&agent, false).is_none());

    for (state, expected) in [
        (
            WorkItemSchedulingState::Runnable,
            AgentSchedulingPosture::HasRunnableWork,
        ),
        (
            WorkItemSchedulingState::WaitingTask,
            AgentSchedulingPosture::WaitingForTask,
        ),
        (
            WorkItemSchedulingState::WaitingExternal,
            AgentSchedulingPosture::WaitingForExternal,
        ),
        (
            WorkItemSchedulingState::WaitingOperator,
            AgentSchedulingPosture::WaitingForOperator,
        ),
        (
            WorkItemSchedulingState::Blocked,
            AgentSchedulingPosture::Blocked,
        ),
    ] {
        for current in [false, true] {
            let mut item = item();
            item.scheduling_state = state;
            item.is_current = current;
            let mut queue = WorkQueueReadModel::default();
            match state {
                WorkItemSchedulingState::Runnable => queue.queued_runnable.push(item.clone()),
                WorkItemSchedulingState::WaitingOperator => {
                    queue.waiting_for_operator.push(item.clone())
                }
                WorkItemSchedulingState::Blocked => queue.blocked.push(item.clone()),
                _ => queue.items.push(item.clone()),
            }
            let fields = derive_agent_posture_from_work_queue(&queue, &[]);
            if current || state == WorkItemSchedulingState::Runnable {
                assert_eq!(fields.posture, expected);
                let label = if current {
                    "current WorkItem"
                } else {
                    "queued WorkItem"
                };
                let reason = format!("{label} {} is {state:?}", item.work_item.id);
                assert_eq!(fields.reason.to_string(), reason);
                assert_eq!(item.posture_reason(), reason);
                assert_eq!(fields.work_item_id, Some(item.work_item.id.as_str()));
                let checked = fields.materialize(true)?;
                let ordinary = derive_agent_posture(&agent, false, &queue, &[])?;
                assert_eq!(checked, ordinary);
                assert_eq!(
                    serde_json::to_value(checked)?,
                    serde_json::to_value(ordinary)?
                );
            } else {
                assert_eq!(fields.posture, AgentSchedulingPosture::Idle);
                assert_eq!(
                    fields.reason.to_string(),
                    "no queued input, active turn, runnable work, or active waits"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn snapshot_posture_uses_selected_state_without_redecoding_storage() -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut selected = AgentState::new("web");
    selected.status = AgentStatus::AwakeIdle;
    db.agent_states().upsert_for_fixture(&selected)?;
    db.connection()?.execute(
        "UPDATE agent_states SET payload_json='{' WHERE agent_id='web'",
        [],
    )?;
    assert!(db.agent_states().latest("web").is_err());
    let view = RuntimeReadModels::borrowed_for_agent(&db, "web")?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let header = phase.finish()?;
    let posture = phase.read(|| view.agent_posture_for_snapshot(&selected))?;
    assert_eq!(posture.posture, AgentSchedulingPosture::Idle);
    let delta = cost(&posture.reason, [None, None, None]);
    assert_eq!(phase.finish()?, (header.0 + delta.0, header.1 + delta.1));
    drop((posture, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scheduler_posture_kernel_uses_original_allowance_before_allocating_fields() -> Result<()> {
    let mut agent = AgentState::new("web");
    agent.status = AgentStatus::AwakeIdle;
    agent.current_run_id = Some("run_汉🦀".into());
    agent.current_turn_work_item_id = Some("work_汉🦀".into());
    let queue = WorkQueueReadModel::default();
    let reason = "agent has an active turn";
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let header = ResourceReadMaterial::new(&ledger, &scope())?.finish()?;
    let delta = cost(
        reason,
        [
            agent.current_turn_work_item_id.as_deref(),
            None,
            agent.current_run_id.as_deref(),
        ],
    );
    let full = (header.0 + delta.0, header.1 + delta.1);
    for (bytes, records, accepts) in [
        (full.0, full.1, true),
        (full.0 - 1, full.1, false),
        (full.0, full.1 - 1, false),
    ] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
        let result = phase.read(|| derive_agent_posture(&agent, false, &queue, &[]));
        if accepts {
            let posture = result?;
            assert_eq!(posture.reason, reason);
            assert_eq!(posture.work_item_id, agent.current_turn_work_item_id);
            assert_eq!(posture.run_id, agent.current_run_id);
            assert_eq!(phase.finish()?, full);
        } else {
            assert!(format!("{:#}", result.unwrap_err()).contains("ResourceExhausted"));
            assert!(phase.finish().is_err());
            assert!(phase
                .read(|| derive_agent_posture(&agent, false, &queue, &[]))
                .is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn posture_final_fields_have_exact_cost_and_atomic_bytes_records_refusal() -> Result<()> {
    let item = item();
    let work = item.work_item.id.as_str();
    let task = "task_汉字\0🦀";
    let run = "run_🦀";
    let reason = format!("current WorkItem {work} is {:?}", item.scheduling_state);
    for mask in 0..8 {
        let ids = [
            (mask & 1 != 0).then_some(work),
            (mask & 2 != 0).then_some(task),
            (mask & 4 != 0).then_some(run),
        ];
        let fields = || AgentPostureFields {
            posture: AgentSchedulingPosture::WaitingForTask,
            reason: PostureReason::WorkItem(&item),
            work_item_id: ids[0],
            task_id: ids[1],
            run_id: ids[2],
        };
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let header = phase.finish()?;
        let delta = cost(&reason, ids);
        let full = (header.0 + delta.0, header.1 + delta.1);
        let projection = {
            let _entry = phase.enter();
            let borrowed = fields();
            assert_eq!(phase.finish()?, header);
            borrowed.materialize(true)?
        };
        assert_eq!(phase.finish()?, full);
        assert_eq!(projection.reason, reason);
        assert_eq!(projection.reason.capacity(), reason.len());
        assert_eq!(projection.work_item_id.as_deref(), ids[0]);
        assert_eq!(projection.task_id.as_deref(), ids[1]);
        assert_eq!(projection.run_id.as_deref(), ids[2]);
        drop(projection);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);

        for exact in [true, false] {
            for records in [false, true] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    full.0 - u64::from(!exact && !records),
                    full.1 - usize::from(!exact && records),
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    assert!(Arc::ptr_eq(
                        &ResourceReadMaterial::active_guard()?.unwrap(),
                        &holder
                    ));
                    if exact {
                        assert_eq!(fields().materialize(true)?.reason, reason);
                        assert_eq!(phase.finish()?, full);
                    } else {
                        for _ in 0..2 {
                            let error = fields().materialize(true).unwrap_err();
                            assert!(
                                format!("{error:#}").contains("ResourceExhausted"),
                                "{error:#}"
                            );
                            assert_eq!(ledger.report()?.observed_bytes, header.0);
                            assert!(phase.finish().is_err());
                            assert!(ResourceReadMaterial::active_guard().is_err());
                        }
                    }
                }
                drop(phase);
                assert_eq!(
                    ledger.report()?.observed_bytes,
                    if exact { full.0 } else { header.0 }
                );
                drop(holder);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn ordinary_posture_api_does_not_use_snapshot_material_admission() -> Result<()> {
    let agent = AgentState::new("web");
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let header = phase.finish()?;
    {
        let _entry = phase.enter();
        let fields = derive_agent_posture_before_work_queue(&agent, true).unwrap();
        let projection = fields.materialize(false)?;
        assert_eq!(projection.reason, "agent has queued input");
        assert_eq!(phase.finish()?, header);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_posture_borrows_only_first_active_matching_task_subject() -> Result<()> {
    let mut item = item();
    item.scheduling_state = WorkItemSchedulingState::WaitingTask;
    let mut queue = WorkQueueReadModel::default();
    queue.items.push(item.clone());
    let now = Utc::now();
    let matching = WaitConditionRecord {
        id: "wait".into(),
        agent_id: "web".into(),
        work_item_id: Some(item.work_item.id.clone()),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Task,
        source: None,
        subject_ref: Some("task_汉字".into()),
        waiting_for: "task".into(),
        wake_sources: Vec::new(),
        continuation: None,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    };
    let mut wrong_work = matching.clone();
    wrong_work.work_item_id = Some("other".into());
    let mut wrong_kind = matching.clone();
    wrong_kind.kind = WaitConditionKind::Operator;
    let mut inactive = matching.clone();
    inactive.status = WaitConditionStatus::Triggered;
    let mut later = matching.clone();
    later.subject_ref = Some("later".into());
    let waits = [wrong_work, wrong_kind, inactive, matching, later];
    let fields = derive_agent_posture_from_work_queue(&queue, &waits);
    assert_eq!(fields.posture, AgentSchedulingPosture::WaitingForTask);
    assert_eq!(
        fields.task_id.unwrap().as_ptr(),
        waits[3].subject_ref.as_ref().unwrap().as_ptr()
    );
    let projection = fields.materialize(true)?;
    assert_eq!(projection.task_id.as_deref(), Some("task_汉字"));
    let fields = derive_agent_posture_from_work_queue(&queue, &[]);
    assert!(fields.task_id.is_none());
    Ok(())
}
