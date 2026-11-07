use super::*;
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use crate::runtime_db::scheduling_snapshot::{
    prepare_cold_scheduling_query_tx, SchedulingDomainLoadRequest, SchedulingSnapshotError,
};
use crate::types::AuditEvent;

fn query_request() -> SchedulingDomainLoadRequest {
    SchedulingDomainLoadRequest {
        capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    }
}

struct FailedColdStateProvider;

#[async_trait]
impl AgentProvider for FailedColdStateProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        anyhow::bail!("cold State provider transport failure")
    }
}

#[tokio::test]
async fn cold_operator_state_after_provider_failure_and_stop_keeps_retry_work() -> Result<()> {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_home, initial) = test_host();
    let config = initial.config().as_ref().clone();
    initial.shutdown().await?;
    drop(initial);
    let host = RuntimeHost::new_with_provider(config.clone(), Arc::new(FailedColdStateProvider))?;
    let agent_id = config.default_agent_id.clone();
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    runtime.control(ControlAction::Stop).await?;
    let work = runtime
        .create_work_item(
            "retry after provider failure".into(),
            None,
            None,
            Vec::new(),
        )
        .await?;
    runtime.pick_work_item(work.id.clone()).await?;
    host.control_public_agent(&agent_id, ControlAction::Start)
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if host
                .runtime_db()
                .agent_states()
                .latest(&agent_id)?
                .is_some_and(|state| state.last_runtime_failure.is_some())
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    host.control_public_agent(&agent_id, ControlAction::Stop)
        .await?;
    host.shutdown().await?;
    drop((runtime, host));

    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let read = host
        .operator_agent_state_projection(&agent_id, 50, 50)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert!(matches!(read.source, AgentStateProjectionSource::Storage));
    assert_eq!(read.agent.agent.status, AgentStatus::Stopped);
    assert!(read.agent.agent.last_runtime_failure.is_some());
    assert_eq!(
        read.agent.work_queue.current.as_ref().map(|item| &item.id),
        Some(&work.id)
    );
    let held = host.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let first = Arc::new(read);
    let last = first.clone();
    drop(first);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    drop(last);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let response = crate::http::router(crate::http::AppState::for_tcp(host.clone()))
        .oneshot(
            Request::builder()
                .uri(format!("/api/agents/{agent_id}/state"))
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_scheduler_error_posture_never_reads_event_bodies() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;

    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let state = AgentState::new(&identity.agent_id);
    host.runtime_db()
        .agent_states()
        .initialize_cold_for_fixture(&state, identity.incarnation)?;
    host.runtime_db().audit_events().append(
        Some(&identity.agent_id),
        &AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({"unused": vec![0; 9000]}),
        ),
    )?;
    let connection = host.runtime_db().connection()?;
    connection.execute_batch(
        "ALTER TABLE audit_events RENAME TO cold_event_bodies_forbidden;
         CREATE VIEW audit_events AS
         SELECT agent_id,kind,created_at,event_seq FROM cold_event_bodies_forbidden;",
    )?;
    drop(connection);
    assert!(host
        .runtime_db()
        .audit_events()
        .recent(Some(&identity.agent_id), 64)
        .is_err());
    let storage = host
        .agent_storage_read_only(&identity.agent_id)
        .map_err(|error| anyhow!("{error:?}"))?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let phase = ResourceReadMaterial::new(&host.inner.input_evidence_budget, &scope)?;
    let projection = crate::runtime::SchedulerProjection::from_state_with_scheduling_observation(
        &storage,
        &state,
        Default::default(),
        0,
        &phase,
    )?;
    assert!(projection.runtime_error);
    phase.finish()?;
    let first = phase.guard()?;
    let last = first.clone();
    let held = host.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    drop((phase, projection, first));
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    drop(last);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let failed = ResourceReadMaterial::with_test_limits(
        &host.inner.input_evidence_budget,
        &scope,
        64 * 1024 * 1024,
        64,
    )?;
    for _ in 0..2 {
        assert!(
            crate::runtime::SchedulerProjection::from_state_with_scheduling_observation(
                &storage,
                &state,
                Default::default(),
                0,
                &failed,
            )
            .is_err()
        );
        assert!(failed.finish().is_err());
    }
    drop(failed);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_lightweight_owned_state_keeps_rich_fields_and_original_holder() -> Result<()> {
    for status in [
        AgentStatus::AwakeIdle,
        AgentStatus::Asleep,
        AgentStatus::Stopped,
    ] {
        let (_home, host) = test_host();
        let identity = host.ensure_default_agent_identity()?;
        let mut state = AgentState::new(&identity.agent_id);
        state.status = status.clone();
        state.timezone_override = Some("UTC".into());
        state.last_wake_reason = Some("owned State 汉🦀".repeat(97));
        state.working_memory.current_working_memory.recent_decisions =
            vec!["first".into(), "nested State 汉🦀".repeat(113)];
        state.working_memory.active_episode_id = Some("episode-owned-state".into());
        state.total_input_tokens = 123;
        state.total_output_tokens = 45;
        state.total_model_rounds = 6;
        state.turn_index = 71;
        state.attached_workspaces = vec!["workspace-owned-state".into()];
        host.runtime_db()
            .agent_states()
            .initialize_cold_for_fixture(&state, identity.incarnation)?;
        let expected_wire = serde_json::to_value(&state)?;
        let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
        let projection = host
            .local_agent_lightweight_projection(&identity.agent_id)
            .await
            .map_err(|error| anyhow!("{error:?}"))?;
        assert_eq!(projection.agent, state);
        assert_eq!(
            projection.lifecycle,
            AgentLifecycleHint::from_status(&identity.agent_id, status)
        );
        assert!(projection._scheduling_material.is_some());
        assert!(projection._work_budget.is_none());
        assert_eq!(projection.active_task_count, 0);
        assert!(projection.active_children.is_empty());
        let held = host.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(held > baseline);
        let first = Arc::new(projection);
        let last = first.clone();
        drop(first);
        assert_eq!(serde_json::to_value(&last.agent)?, expected_wire);
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
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cold_lightweight_owned_state_keeps_empty_fallback_and_rejection() -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let projection = host
        .local_agent_lightweight_projection(&identity.agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(projection.agent, stopped_unloaded_agent(&identity.agent_id));
    assert!(!projection.lifecycle.accepts_external_messages);
    assert!(projection._scheduling_material.is_some());
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(projection);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let timer = TimerRecord {
        calendar: None,
        id: "raw-lightweight-timer".into(),
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
        .local_agent_lightweight_projection(&identity.agent_id)
        .await
        .err()
        .context("unqualified original Scheduling must not become empty State")?;
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
async fn cold_host_observed_trigger_borrow_preserves_all_three_consumers_and_holder() -> Result<()>
{
    let (_home, host) = test_host();
    let config = host.config().as_ref().clone();
    let runtime = host.default_runtime().await?;
    let capability = runtime
        .default_external_trigger(crate::types::CallbackDeliveryMode::WakeHint)
        .await?;
    let id = runtime.instance_agent_id().to_owned();
    let expected = runtime
        .storage()
        .latest_external_triggers()?
        .into_iter()
        .find(|row| row.external_trigger_id == capability.external_trigger_id)
        .context("fixture trigger")?;
    drop(runtime);
    host.shutdown().await?;
    drop(host);

    let cold = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let baseline = cold.inner.input_evidence_budget.report()?.observed_bytes;
    let summary = Arc::new(
        cold.local_agent_summary(&id)
            .await
            .map_err(|e| anyhow!("{e:?}"))?,
    );
    assert_eq!(summary.agent.id, id);
    assert_eq!(summary.active_external_triggers.len(), 1);
    assert_eq!(
        summary.active_external_triggers[0].external_trigger_id,
        expected.external_trigger_id
    );
    let held = cold.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let last = summary.clone();
    drop(summary);
    let wire = serde_json::to_value(last.as_ref())?;
    assert_eq!(
        wire["active_external_triggers"][0]["external_trigger_id"],
        expected.external_trigger_id
    );
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    drop((last, wire));
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );

    let light = cold
        .local_agent_lightweight_projection(&id)
        .await
        .map_err(|e| anyhow!("{e:?}"))?;
    assert_eq!(light.agent.id, id);
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let state_wire = serde_json::to_value(&light.agent)?;
    assert_eq!(state_wire["id"], id);
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    drop((light, state_wire));
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );

    let projection = cold
        .operator_agent_state_projection(&id, 0, 0)
        .await
        .map_err(|e| anyhow!("{e:?}"))?;
    assert_eq!(projection.source, AgentStateProjectionSource::Storage);
    assert_eq!(projection.external_triggers, [expected.clone()]);
    assert!(projection.material._scheduling.is_some());
    let held = cold.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let first = Arc::new(projection);
    let last = first.clone();
    drop(first);
    let wire = serde_json::to_value(&last.external_triggers)?;
    assert_eq!(wire, serde_json::to_value([expected])?);
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    drop((last, wire));
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    cold.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_host_timestamp_order_matches_repository_millis_through_last_holder() -> Result<()> {
    use crate::runtime::resident_scheduling::{SchedulingFamily, SchedulingKey};
    use crate::runtime_db::repositories::{timer_updated_at, timestamp};
    use crate::runtime_db::scheduling_snapshot::{
        prepare_scheduling_write_tx, register_scheduling_writes_tx,
    };
    let (_home, host) = test_host();
    let config = host.config().as_ref().clone();
    let fixture_ledger = host.inner.input_evidence_budget.clone();
    let runtime = host.default_runtime().await?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    let trigger = runtime
        .default_external_trigger(crate::types::CallbackDeliveryMode::WakeHint)
        .await?;
    let agent_id = runtime.instance_agent_id().to_owned();
    let scope = InputEvidenceScope {
        agent_id: agent_id.clone(),
        identity_incarnation: runtime.instance_incarnation(),
    };
    let storage = runtime.storage().clone();
    drop(runtime);
    host.shutdown().await?;
    drop(host);

    // Persist legal fixtures only after the original running owner has stopped.
    let trigger = storage
        .latest_external_triggers()?
        .into_iter()
        .find(|row| row.external_trigger_id == trigger.external_trigger_id)
        .context("fixture canonical trigger")?;
    let ids = ["original", "A", "a", "a\0", "汉", "🦀", "é", "beta", "t"];
    let times = [
        (-1, 1),
        (-1, 999_999),
        (0, 0),
        (1_483_228_799, 1_000_000_000),
        (1_483_228_799, 1_000_999_999),
        (1_483_228_799, 1_001_000_000),
        (1_483_228_800, 0),
        (1_483_228_800, 123_000_001),
        (1_483_228_800, 123_999_999),
    ]
    .map(|(seconds, nanos)| chrono::DateTime::from_timestamp(seconds, nanos).unwrap());
    let mut timers = Vec::new();
    let mut triggers = Vec::new();
    for (index, id) in ids.into_iter().enumerate() {
        let mut row = timer.clone();
        if index > 0 {
            row.id = id.into();
        }
        // Advance the original row even when the fixture timestamp is older.
        row.fire_count += 1;
        row.created_at = times[index];
        row.last_fired_at = (index % 3 == 0).then_some(times[(index + 1) % times.len()]);
        row.next_fire_at = (index % 3 != 2).then_some(times[(index + 2) % times.len()]);
        timers.push(row);
        let mut row = trigger.clone();
        if index > 0 {
            row.external_trigger_id = id.into();
            row.status = crate::types::ExternalTriggerStatus::Revoked;
            row.revoked_at = Some(times[index]);
        }
        row.delivery_count += 1;
        row.token = Some(format!("fixture-only-{index}"));
        row.token_hash = format!("fixture-only-{index}");
        row.created_at = times[index];
        triggers.push(row);
    }
    let db = storage.runtime_db()?.context("fixture runtime database")?;
    let keys = timers
        .iter()
        .map(|row| SchedulingKey::new(SchedulingFamily::Timer, &row.id))
        .chain(triggers.iter().map(|row| {
            SchedulingKey::new(SchedulingFamily::ExternalTrigger, &row.external_trigger_id)
        }))
        .collect();
    drop(db.transaction(|tx| {
        let request = query_request();
        let proof = prepare_scheduling_write_tx(tx, &scope, &keys, &request, &fixture_ledger)?;
        for row in &timers {
            crate::runtime_db::repositories::upsert_timer_tx(tx, row)?;
        }
        for row in &triggers {
            crate::runtime_db::repositories::upsert_external_trigger_tx(tx, row)?;
        }
        let state = crate::runtime_db::context_inputs::ContextDurableRead::new(tx, &agent_id)
            .scope_facts()?
            .state
            .context("fixture final state")?;
        register_scheduling_writes_tx(tx, &proof, &keys, &state, &request)
    })?);
    assert_eq!(fixture_ledger.report()?.observed_bytes, 0);
    drop(storage);
    timers.sort_by(|a, b| {
        timestamp(timer_updated_at(b))
            .cmp(&timestamp(timer_updated_at(a)))
            .then_with(|| timestamp(b.created_at).cmp(&timestamp(a.created_at)))
            .then_with(|| a.id.cmp(&b.id))
    });
    triggers.sort_by(|a, b| {
        timestamp(b.created_at)
            .cmp(&timestamp(a.created_at))
            .then_with(|| a.external_trigger_id.cmp(&b.external_trigger_id))
    });
    let cold = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let identity = cold.active_agent_identity(&agent_id)?;
    let baseline = cold.inner.input_evidence_budget.report()?.observed_bytes;
    for limit in [0, 1, 3, 9] {
        let mut expected = timers[..limit].to_vec();
        expected.reverse();
        let read = cold.acquire_cold_scheduling_query(&identity, limit).await?;
        assert_eq!(read.timers, expected);
        assert_eq!(read.triggers, triggers);
        let held = cold.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(held > baseline);
        let first = Arc::new(read);
        let last = first.clone();
        drop(first);
        let wire = serde_json::to_value((&last.timers, &last.triggers))?;
        assert_eq!(wire, serde_json::to_value((&expected, &triggers))?);
        assert_eq!(
            cold.inner.input_evidence_budget.report()?.observed_bytes,
            held
        );
        assert!(cold.inner.runtimes.read().await.agents.is_empty());
        drop((wire, last));
        assert_eq!(
            cold.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    cold.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_host_output_vectors_keep_typed_capacity_and_last_original_material() -> Result<()> {
    let (_home, host) = test_host();
    let config = host.config().as_ref().clone();
    let runtime = host.default_runtime().await?;
    for _ in 0..9 {
        runtime
            .schedule_timer(3_600_000, None, Some("cold Vec material".into()))
            .await?;
    }
    runtime
        .default_external_trigger(crate::types::CallbackDeliveryMode::WakeHint)
        .await?;
    let agent_id = runtime.instance_agent_id().to_owned();
    drop(runtime);
    host.shutdown().await?;
    drop(host);
    let cold = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let identity = cold.active_agent_identity(&agent_id)?;
    let baseline = cold.inner.input_evidence_budget.report()?.observed_bytes;
    let read = cold.acquire_cold_scheduling_query(&identity, 3).await?;
    assert_eq!((read.timers.len(), read.timers.capacity()), (3, 3));
    assert_eq!((read.triggers.len(), read.triggers.capacity()), (1, 1));
    let held = cold.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let first = Arc::new(read);
    let last = first.clone();
    drop(first);
    let wire = serde_json::to_value((&last.timers, &last.triggers))?;
    assert_eq!(wire[0].as_array().unwrap().len(), 3);
    assert_eq!(wire[1].as_array().unwrap().len(), 1);
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    drop((wire, last));
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    cold.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_host_projects_actual_qualified_timer_and_trigger_without_activation() -> Result<()> {
    let (_home, host) = test_host();
    let config = host.config().as_ref().clone();
    let runtime = host.default_runtime().await?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("original cold timer".into()))
        .await?;
    let trigger = runtime
        .default_external_trigger(crate::types::CallbackDeliveryMode::WakeHint)
        .await?;
    let scope = InputEvidenceScope {
        agent_id: runtime.instance_agent_id().into(),
        identity_incarnation: runtime.instance_incarnation(),
    };
    drop(runtime);
    host.shutdown().await?;
    drop(host);
    let cold = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    let baseline = cold.inner.input_evidence_budget.report()?.observed_bytes;
    let projection = cold
        .operator_agent_state_projection(&scope.agent_id, 40, 50)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(projection.source, AgentStateProjectionSource::Storage);
    assert_eq!(projection.agent.agent.id, scope.agent_id);
    assert!(projection.timers.iter().any(|row| row == &timer));
    assert!(projection
        .external_triggers
        .iter()
        .any(|row| row.external_trigger_id == trigger.external_trigger_id
            && row.target_agent_id == scope.agent_id
            && row.status == ExternalTriggerStatus::Active));
    assert!(projection.material._scheduling.is_some());
    assert!(cold.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let wire = serde_json::to_value((&projection.timers, &projection.external_triggers))?;
    assert_eq!(wire[0][0]["id"], timer.id);
    assert!(cold.inner.runtimes.read().await.agents.is_empty());
    drop(wire);
    drop(projection);
    assert_eq!(
        cold.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    cold.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn no_state_cold_empty_display_requires_every_original_inventory_empty() -> Result<()> {
    for origin in ["empty", "raw_timer", "qualified", "fixed", "import", "wake"] {
        let (_home, host) = test_host();
        let identity = host.ensure_default_agent_identity()?;
        let scope = InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        };
        assert!(host
            .runtime_db()
            .agent_states()
            .latest(&identity.agent_id)?
            .is_none());
        host.runtime_db().transaction(|tx| {
            match origin {
                "raw_timer"=>{
                    let timer=TimerRecord {
    calendar: None,id:"raw-cold-timer".into(),agent_id:scope.agent_id.clone(),
                        created_at:Utc::now(),duration_ms:3600000,interval_ms:None,repeat:false,
                        status:TimerStatus::Active,summary:None,next_fire_at:None,last_fired_at:None,fire_count:0};
                    crate::runtime_db::repositories::upsert_timer_tx(tx,&timer)?;
                    tx.execute("UPDATE timers SET payload_json='bad-unqualified-body' WHERE timer_id=?1",[&timer.id])?;
                }
                "qualified"=>{tx.execute("INSERT INTO scheduling_domain_records(agent_id,identity_incarnation,family,record_id,revision,availability) VALUES(?1,?2,'timer','known-absent',1,'absent')",rusqlite::params![scope.agent_id,scope.identity_incarnation])?;}
                "fixed"=>{
                    tx.execute("INSERT INTO scheduling_fixed_manifests(inventory_id,agent_id,tables_json,member_count,metadata_bytes,content_digest,sealed) VALUES('cold-fixed',?1,'[\"timer\"]',1,128,?2,0)",rusqlite::params![scope.agent_id,"0".repeat(64)])?;
                    tx.execute("INSERT INTO scheduling_fixed_members(inventory_id,family,record_id,content_digest,projection_digest,status,sort_at,created_at) VALUES('cold-fixed','timer','fixed-absent',?1,?1,'active','2026-10-01T00:00:00Z','2026-10-01T00:00:00Z')",["0".repeat(64)])?;
                    tx.execute("UPDATE scheduling_fixed_manifests SET sealed=1 WHERE inventory_id='cold-fixed'",[])?;
                }
                "import"=>{tx.execute("INSERT INTO scheduling_import_inventory(agent_id,family,record_id,received_digest,content_digest,projection_digest) VALUES(?1,'timer','original-import',?2,?2,?2)",rusqlite::params![scope.agent_id,"0".repeat(64)])?;}
                "wake"=>{
                    let timer=TimerRecord {
    calendar: None,id:"foreign-cold-timer".into(),agent_id:"foreign-agent".into(),
                        created_at:Utc::now(),duration_ms:3600000,interval_ms:None,repeat:false,
                        status:TimerStatus::Active,summary:None,next_fire_at:None,last_fired_at:None,fire_count:1};
                    crate::runtime_db::repositories::upsert_timer_tx(tx,&timer)?;
                    let message=MessageEnvelope::new(scope.agent_id.clone(),MessageKind::TimerTick,
                        MessageOrigin::Timer {timer_id:timer.id.clone()},AuthorityClass::RuntimeInstruction,
                        Priority::Next,MessageBody::Text {text:"original owned wake source".into()});
                    let (message,_)=crate::runtime_db::evidence::append_message_tx(tx,&message)?;
                    tx.execute("INSERT INTO timer_wakes(timer_id,message_id,fire_count,status,created_at,updated_at) VALUES(?1,?2,1,'pending',?3,?3)",rusqlite::params![timer.id,message.id,crate::runtime_db::repositories::timestamp(message.created_at)])?;
                }
                _=>{}
            }
            Ok(())
        })?;
        let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
        let result = host
            .operator_agent_state_projection(&identity.agent_id, 10, 10)
            .await;
        if origin == "empty" {
            let projection = result.map_err(|error| anyhow!("{error:?}"))?;
            assert_eq!(projection.agent.agent.status, AgentStatus::Stopped);
            assert!(projection.timers.is_empty() && projection.external_triggers.is_empty());
            assert!(projection.material._scheduling.is_some());
            drop(projection);
        } else {
            let error = result
                .err()
                .context("known original Scheduling cannot become an empty display")?;
            assert!(
                matches!(error,PublicAgentError::Runtime(ref error)
                if matches!(error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
                    Some(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive))),
                "{origin}: {error:?}"
            );
        }
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        let counts:(u64,u64)=host.runtime_db().connection()?.query_row(
            "SELECT (SELECT COUNT(*) FROM scheduling_domain_current),(SELECT COUNT(*) FROM agent_states)",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(
            counts,
            (0, 0),
            "no State/root may be fabricated for {origin}"
        );
    }
    Ok(())
}
#[test]
fn cold_preparation_native_state_budget_and_retired_scope_precede_corrupt_decode() -> Result<()> {
    let (_home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let mut state = serde_json::to_value(AgentState::new(&identity.agent_id))?;
    state["pending"] = serde_json::json!("corrupt-count");
    state["working_memory"] = serde_json::json!({"large":vec![0;4096]});
    host.runtime_db().transaction(|tx| {
        tx.execute("INSERT INTO agent_states(agent_id,status,current_run_id,updated_at,payload_json) VALUES(?1,?2,NULL,?3,?4)",rusqlite::params![scope.agent_id,state["status"].as_str().context("fixture State status absent")?,crate::runtime_db::repositories::timestamp(Utc::now()),serde_json::to_string(&state)?])?;
        Ok(())
    })?;
    let ledger = &host.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let mut small = query_request();
    small.record_limit = 64;
    let error = host
        .runtime_db()
        .transaction(|tx| prepare_cold_scheduling_query_tx(tx, &scope, &small, ledger))
        .err()
        .context("native node gate must precede corrupt State decoder")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    host.runtime_db().transaction(|tx| {
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&scope.agent_id])?;
        Ok(())
    })?;
    let error = host
        .runtime_db()
        .transaction(|tx| prepare_cold_scheduling_query_tx(tx, &scope, &query_request(), ledger))
        .err()
        .context("old scope cannot inspect corrupt successor State")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::RetiredScope)
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
