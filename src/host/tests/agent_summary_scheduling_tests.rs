use super::*;
use crate::types::CallbackDeliveryMode;

#[tokio::test]
async fn cold_summary_owned_queue_preserves_rich_closure_posture_and_last_holder() -> Result<()> {
    use crate::types::{
        TodoItem, TodoItemState, WorkItemPlanStatus, WorkItemRecord, WorkItemState,
    };

    for current in [Some("ready"), Some("blocked"), None] {
        let (_home, host) = test_host();
        let identity = host.ensure_default_agent_identity()?;
        let mut state = AgentState::new(&identity.agent_id);
        state.status = AgentStatus::AwakeIdle;
        state.turn_index = 7;
        state.working_memory.current_working_memory.recent_decisions =
            vec!["summary State 汉🦀".repeat(31)];
        host.runtime_db()
            .agent_states()
            .initialize_cold_for_fixture(&state, identity.incarnation)?;
        let mut records = Vec::new();
        for id in ["ready", "queued", "blocked", "completed"] {
            let mut item = WorkItemRecord::new(
                &identity.agent_id,
                "summary queue 汉🦀 \"{}[],:\\ ".repeat(53),
                if id == "completed" {
                    WorkItemState::Completed
                } else {
                    WorkItemState::Open
                },
            );
            item.id = id.into();
            item.plan_status = WorkItemPlanStatus::Ready;
            item.todo_list = vec![TodoItem {
                text: "nested Todo 汉🦀".repeat(47),
                state: TodoItemState::InProgress,
            }];
            if id == "blocked" {
                item.blocked_by = Some("outside dependency 汉🦀".repeat(29));
            }
            assert!(host
                .runtime_db()
                .work_items()
                .insert_new_qualified_for_fixture(&item, identity.incarnation)?);
            records.push(item);
        }
        // Set focus only after every target has been written and qualified.
        state.current_work_item_id = current.map(str::to_owned);
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&state)?;
        let storage = host
            .agent_storage_read_only(&identity.agent_id)
            .map_err(|error| anyhow!("{error:?}"))?;
        // Preserve the former owning-clone input as the behavioral oracle.
        let queue = storage.work_queue_with_observed_scheduling(&state, &[])?;
        assert_eq!(queue.current.as_ref().map(|item| item.id.as_str()), current);
        assert!(!queue.queued_runnable.is_empty());
        assert!(!queue.blocked.is_empty());
        assert_eq!(queue.completed_recent.len(), 1);
        assert_eq!(queue.items.len(), records.len());
        for projected in &queue.items {
            assert_eq!(
                &projected.work_item,
                records
                    .iter()
                    .find(|item| item.id == projected.work_item.id)
                    .unwrap()
            );
        }
        let posture = storage.agent_posture_projection_with_work_queue(&state, &queue)?;
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
        let summary = host
            .local_agent_summary(&identity.agent_id)
            .await
            .map_err(|error| anyhow!("{error:?}"))?;
        assert_eq!(summary.agent, state);
        assert_eq!(summary.scheduling_posture, posture);
        assert_eq!(summary.closure, closure);
        assert_eq!(summary.active_task_count, 0);
        assert!(summary.active_external_triggers.is_empty());
        assert!(summary.active_wait_conditions.is_empty());
        let expected_wire = serde_json::to_value(&*summary)?;
        let held = host.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(held > baseline);
        let first = Arc::new(summary);
        let last = first.clone();
        drop(first);
        assert_eq!(serde_json::to_value(last.as_ref())?, expected_wire);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            held
        );
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        drop(last);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert_eq!(
            host.runtime_db()
                .agent_states()
                .latest(&identity.agent_id)?,
            Some(state)
        );
        for record in records {
            assert_eq!(
                host.runtime_db().work_items().latest(&record.id)?,
                Some(record)
            );
        }
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cold_summary_owned_queue_keeps_empty_fallback_and_scheduling_rejection() -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let summary = host
        .local_agent_summary(&identity.agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(summary.agent, stopped_unloaded_agent(&identity.agent_id));
    assert!(!summary.lifecycle.accepts_external_messages);
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(summary);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let timer = TimerRecord {
        calendar: None,
        id: "raw-summary-queue-timer".into(),
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
    let error = host
        .local_agent_summary(&identity.agent_id)
        .await
        .err()
        .context("unqualified Scheduling must not become an empty summary")?;
    assert!(
        matches!(error, PublicAgentError::Runtime(ref error)
        if matches!(
            error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
            Some(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive)
        )),
        "{error:?}"
    );
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_summary_and_compact_http_projection_keep_exact_scheduling_until_wire() -> Result<()> {
    let (_home, host) = test_host();
    let config = host.config().as_ref().clone();
    let runtime = host.default_runtime().await?;
    let trigger = runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    let agent_id = runtime.instance_agent_id().to_owned();
    drop(runtime);
    host.shutdown().await?;
    drop(host);
    let cold = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    let baseline = cold.inner.input_evidence_budget.report()?.observed_bytes;
    let summary = cold
        .local_agent_summary(&agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(summary.active_external_triggers.len(), 1);
    assert_eq!(
        summary.active_external_triggers[0].external_trigger_id,
        trigger.external_trigger_id
    );
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let original = serde_json::to_value(&*summary)?;
    let wire = serde_json::to_value(&summary)?;
    assert_eq!(wire, original);
    assert_eq!(wire["agent"]["id"], agent_id);
    // This is the actual HTTP synchronous serializer, consuming the carrier.
    let response =
        crate::http::traced_json("summary-owned-test", std::time::Instant::now(), summary)
            .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let compact = cold
        .local_agent_lightweight_projection(&agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert!(compact._scheduling_material.is_some());
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let response = crate::http::traced_json(
        "compact-owned-test",
        std::time::Instant::now(),
        crate::http_dto::SlimAgentDto::from(&compact),
    )
    .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(compact);
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    cold.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_summary_no_state_known_timer_is_typed_and_never_becomes_empty_ready() -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    host.runtime_db().transaction(|tx| {
        tx.execute("INSERT INTO scheduling_domain_records(agent_id,identity_incarnation,family,record_id,revision,availability) VALUES(?1,?2,'timer','summary-known-absent',1,'absent')",
            rusqlite::params![identity.agent_id, identity.incarnation])?;
        Ok(())
    })?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    for full in [true, false] {
        let error = if full {
            host.local_agent_summary(&identity.agent_id).await.err()
        } else {
            host.local_agent_lightweight_projection(&identity.agent_id)
                .await
                .err()
        }
        .context("known NoState Scheduling cannot display empty")?;
        assert!(
            matches!(error,PublicAgentError::Runtime(ref error)
            if matches!(error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
                Some(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive))),
            "{error:?}"
        );
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    let current: usize = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM scheduling_domain_current WHERE agent_id=?1",
        [&identity.agent_id],
        |row| row.get(0),
    )?;
    assert_eq!(current, 0);
    host.shutdown().await?;
    Ok(())
}
