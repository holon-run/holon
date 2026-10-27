use super::*;
use crate::types::{TodoItem, TodoItemState, WorkItemPlanStatus, WorkItemRecord, WorkItemState};

fn queue_wire(
    queue: &crate::work_item_scheduling::WorkQueueReadModel,
) -> Result<serde_json::Value> {
    Ok(serde_json::to_value((
        &queue.current,
        &queue.queued_blocked,
        &queue.items,
        &queue.current_runnable,
        &queue.triggered_blocked,
        &queue.queued_runnable,
        &queue.yielded,
        &queue.waiting_for_operator,
        &queue.blocked,
        &queue.completing,
        &queue.completed_recent,
    ))?)
}

#[tokio::test]
async fn cold_borrowed_queue_dual_consumers_match_owned_oracle_and_last_holder() -> Result<()> {
    for current in [Some("ready"), Some("blocked"), None] {
        let (_home, host) = test_host();
        let identity = host.ensure_default_agent_identity()?;
        let mut state = AgentState::new(&identity.agent_id);
        state.status = AgentStatus::AwakeIdle;
        state.turn_index = 11;
        state.current_work_item_id = current.map(str::to_owned);
        for id in ["ready", "queued", "blocked", "completed"] {
            let mut record = WorkItemRecord::new(
                &identity.agent_id,
                "queue 汉🦀 \"{}[],:\\ ".repeat(59),
                if id == "completed" {
                    WorkItemState::Completed
                } else {
                    WorkItemState::Open
                },
            );
            record.id = id.into();
            record.plan_status = WorkItemPlanStatus::Ready;
            record.todo_list = vec![TodoItem {
                text: "retained nested Todo 汉🦀".repeat(43),
                state: TodoItemState::InProgress,
            }];
            if id == "blocked" {
                record.blocked_by = Some("external dependency 汉🦀".repeat(31));
            }
            assert!(host.runtime_db().work_items().insert_new(&record)?);
        }
        host.runtime_db()
            .agent_states()
            .initialize_cold_for_fixture(&state, identity.incarnation)?;
        let storage = host
            .agent_storage_read_only(&identity.agent_id)
            .map_err(|error| anyhow!("{error:?}"))?;
        let queue = storage.work_queue_with_observed_scheduling(&state, &[])?;
        assert_eq!(queue.items.len(), 4);
        assert!(!queue.queued_runnable.is_empty());
        assert!(!queue.blocked.is_empty());
        assert_eq!(queue.completed_recent.len(), 1);
        assert_eq!(
            queue.current.as_ref().map(|record| record.id.as_str()),
            current
        );
        let expected_queue = queue_wire(&queue)?;
        let posture = storage.agent_posture_projection_with_work_queue(&state, &queue)?;
        // The former whole-queue clone path remains the behavioral oracle.
        let material =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new_for_identity(
                &host.inner.input_evidence_budget,
                &identity.agent_id,
                identity.incarnation,
            )?;
        let scheduler =
            crate::runtime::SchedulerProjection::from_state_with_scheduling_observation(
                &storage,
                &state,
                queue.clone(),
                0,
                &material,
            )?;
        let closure =
            RuntimeHandle::closure_decision_from_projection(&state, &scheduler, None, &material)?;
        let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
        let light = host
            .local_agent_lightweight_projection(&identity.agent_id)
            .await
            .map_err(|error| anyhow!("{error:?}"))?;
        assert_eq!(light.agent, state);
        assert_eq!(light.scheduling_posture, posture);
        assert_eq!(light.closure, closure);
        assert_eq!(queue_wire(&light.work_queue)?, expected_queue);
        assert!(light._scheduling_material.is_some());
        let held = host.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(held > baseline);
        let first = Arc::new(light);
        let last = first.clone();
        drop(first);
        assert_eq!(queue_wire(&last.work_queue)?, expected_queue);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            held
        );
        drop(last);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );

        let full = host
            .operator_agent_state_projection(&identity.agent_id, 0, 0)
            .await
            .map_err(|error| anyhow!("{error:?}"))?;
        assert_eq!(full.source, AgentStateProjectionSource::Storage);
        assert_eq!(full.agent.agent, state);
        assert_eq!(full.agent.scheduling_posture, posture);
        assert_eq!(full.agent.closure, closure);
        assert_eq!(queue_wire(&full.agent.work_queue)?, expected_queue);
        assert!(full.material._scheduling.is_some());
        let held = host.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(held > baseline);
        let first = Arc::new(full);
        let last = first.clone();
        drop(first);
        assert_eq!(queue_wire(&last.agent.work_queue)?, expected_queue);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            held
        );
        drop(last);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        assert_eq!(
            host.runtime_db()
                .agent_states()
                .latest(&identity.agent_id)?,
            Some(state)
        );
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cold_borrowed_queue_dual_consumers_keep_empty_fallback_and_rejection() -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let light = host
        .local_agent_lightweight_projection(&identity.agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(light.agent, stopped_unloaded_agent(&identity.agent_id));
    assert_eq!(
        queue_wire(&light.work_queue)?,
        queue_wire(&Default::default())?
    );
    drop(light);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let full = host
        .operator_agent_state_projection(&identity.agent_id, 0, 0)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(full.agent.agent, stopped_unloaded_agent(&identity.agent_id));
    assert_eq!(
        queue_wire(&full.agent.work_queue)?,
        queue_wire(&Default::default())?
    );
    drop(full);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );

    let timer = TimerRecord {
        calendar: None,
        id: "raw-borrowed-queue-timer".into(),
        agent_id: identity.agent_id.clone(),
        created_at: Utc::now(),
        duration_ms: 3_600_000,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: None,
        last_fired_at: None,
        fire_count: 0,
    };
    host.runtime_db().transaction(|tx| {
        crate::runtime_db::repositories::upsert_timer_tx(tx, &timer)?;
        Ok(())
    })?;
    for full in [false, true] {
        let result = if full {
            host.operator_agent_state_projection(&identity.agent_id, 0, 0)
                .await
                .map(|_| ())
        } else {
            host.local_agent_lightweight_projection(&identity.agent_id)
                .await
                .map(|_| ())
        };
        let error = result
            .err()
            .context("raw Scheduling must not become empty queue")?;
        assert!(
            matches!(error, PublicAgentError::Runtime(ref error)
            if matches!(error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
                Some(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive))),
            "{full}: {error:?}"
        );
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    host.shutdown().await?;
    Ok(())
}
