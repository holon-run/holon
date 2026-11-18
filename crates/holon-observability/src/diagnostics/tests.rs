use super::*;

#[test]
fn metric_snapshot_reports_bounded_histogram_quantiles() {
    let metric = MetricAccumulator::new("test.metric");
    for elapsed_ms in [1, 2, 3, 4, 5, 100, 1_500, 70_000] {
        metric.record(Duration::from_millis(elapsed_ms), None);
    }

    let snapshot = metric.snapshot(false);

    assert_eq!(snapshot.count, 8);
    assert_eq!(snapshot.p50_ms, 4);
    assert_eq!(snapshot.p95_ms, 120_000);
    assert_eq!(snapshot.p99_ms, 120_000);
}

#[test]
fn metric_snapshot_deserializes_without_quantiles() {
    let snapshot: MetricSnapshot = serde_json::from_value(serde_json::json!({
        "name": "legacy.metric",
        "count": 1,
        "total_ms": 3,
        "max_ms": 3,
        "avg_ms": 3.0
    }))
    .expect("legacy metric snapshot should deserialize");

    assert_eq!(snapshot.p50_ms, 0);
    assert_eq!(snapshot.p95_ms, 0);
    assert_eq!(snapshot.p99_ms, 0);
}

#[test]
fn snapshot_includes_bounded_runtime_hotspot_groups() {
    record_http_json_response("/agents/{agent_id}/state", Duration::from_millis(7), 1024);
    record_agent_summary_projection(Duration::from_millis(3));
    record_runtime_projection_cache_rebuild();
    record_runtime_projection_cache_read();
    record_runtime_db_connection_open(Duration::from_millis(2));
    record_scheduler_poll("idle", Duration::from_millis(1));

    let snapshot = performance_snapshot();

    assert_eq!(
        snapshot.diagnostics_writer,
        crate::diagnostics_store::writer_stats()
    );
    assert!(snapshot
        .http
        .iter()
        .any(|metric| metric.name == "http.json./agents/{agent_id}/state" && metric.count >= 1));
    assert!(snapshot
        .projections
        .iter()
        .any(|metric| metric.name == "projection.agent_summary" && metric.count >= 1));
    assert!(snapshot.projections.iter().any(|metric| {
        metric.name == "projection.runtime_current_cache.rebuild" && metric.count >= 1
    }));
    assert!(snapshot.projections.iter().any(|metric| {
        metric.name == "projection.runtime_current_cache.read" && metric.count >= 1
    }));
    assert!(snapshot
        .db
        .iter()
        .any(|metric| metric.name == "db.connection.open" && metric.count >= 1));
    assert!(snapshot
        .scheduler
        .iter()
        .any(|metric| metric.name == "scheduler.poll.idle" && metric.count >= 1));
}

#[test]
fn snapshot_includes_scheduler_queue_wait_trigger_groups() {
    for trigger_kind in [
        "operator_input",
        "task_result",
        "external_event",
        "timer_fire",
        "internal_followup",
        "system_tick",
        "unrecognized",
    ] {
        record_scheduler_queue_wait(trigger_kind, Duration::from_millis(10));
    }

    let snapshot = performance_snapshot();
    for name in [
        "scheduler.queue_wait.operator_input",
        "scheduler.queue_wait.task_result",
        "scheduler.queue_wait.external_event",
        "scheduler.queue_wait.timer_fire",
        "scheduler.queue_wait.internal_followup",
        "scheduler.queue_wait.system_tick",
        "scheduler.queue_wait.unknown",
    ] {
        assert!(snapshot
            .scheduler
            .iter()
            .any(|metric| metric.name == name && metric.count >= 1));
    }
}

#[test]
fn snapshot_includes_turn_and_provider_metrics() {
    record_turn_total(Duration::from_millis(100));
    record_turn_context_build(Duration::from_millis(10));
    record_turn_provider_round(Duration::from_millis(50));
    record_turn_tool_execution(Duration::from_millis(30));
    record_turn_cleanup(Duration::from_millis(5));
    record_provider_request_build(Duration::from_millis(5));
    record_provider_round_total(Duration::from_millis(50));
    record_provider_retry(Duration::from_millis(3));
    record_tool_execution("ExecCommand", Duration::from_millis(20), Some(512));
    record_storage_append_event(Duration::from_millis(1));
    record_storage_persist_state(Duration::from_millis(2));
    record_projection_state_agent(Duration::from_millis(4));
    record_projection_state_tasks(Duration::from_millis(3));
    record_projection_state_timers(Duration::from_millis(1));
    record_projection_state_work_items(Duration::from_millis(2));
    record_projection_state_waiting_intents(Duration::from_millis(1));
    record_projection_state_external_triggers(Duration::from_millis(1));
    record_projection_state_workspace(Duration::from_millis(1));
    record_projection_state_serialization(Duration::from_millis(1));
    record_projection_state_source_loaded();
    record_projection_state_source_storage();
    record_projection_state_runtime_spawn_avoided();
    record_projection_agents_list(Duration::from_millis(10));
    record_projection_gate_cache_hit();
    record_projection_gate_cache_miss();
    record_projection_gate_joined_waiter();
    record_projection_gate_rejected();
    record_projection_gate_failed();
    record_projection_gate_cancelled();
    record_projection_gate_leader_started();
    record_projection_gate_leader_finished();

    let snapshot = performance_snapshot();

    assert!(snapshot
        .turn
        .iter()
        .any(|metric| metric.name == "turn.total" && metric.count >= 1));
    assert!(snapshot
        .turn
        .iter()
        .any(|metric| metric.name == "turn.context_build" && metric.count >= 1));
    assert!(snapshot
        .turn
        .iter()
        .any(|metric| metric.name == "turn.provider_round" && metric.count >= 1));
    assert!(snapshot
        .turn
        .iter()
        .any(|metric| metric.name == "turn.tool_execution" && metric.count >= 1));
    assert!(snapshot
        .turn
        .iter()
        .any(|metric| metric.name == "turn.cleanup" && metric.count >= 1));
    assert!(snapshot
        .provider
        .iter()
        .any(|metric| metric.name == "provider.request_build" && metric.count >= 1));
    assert!(snapshot
        .provider
        .iter()
        .any(|metric| metric.name == "provider.round_total" && metric.count >= 1));
    assert!(snapshot
        .provider
        .iter()
        .any(|metric| metric.name == "provider.retry" && metric.count >= 1));
    assert!(snapshot.tools.iter().any(|metric| {
        metric.name == "tool.execution"
            && metric.count >= 1
            && metric.total_bytes.unwrap_or_default() >= 512
    }));
    assert!(snapshot.projection_gate.leaders >= 1);
    assert!(snapshot.projection_gate.joined_waiters >= 1);
    assert!(snapshot.projection_gate.cache_hits >= 1);
    assert!(snapshot.projection_gate.cache_misses >= 1);
    assert!(snapshot.projection_gate.rejected >= 1);
    assert!(snapshot.projection_gate.failed >= 1);
    assert!(snapshot.projection_gate.cancelled >= 1);
    assert_eq!(snapshot.projection_gate.active_permits, 0);
    assert!(snapshot.projection_gate.max_active_permits >= 1);
    for name in [
        "projection.agents_list",
        "projection.agent_state.agent",
        "projection.agent_state.tasks",
        "projection.agent_state.timers",
        "projection.agent_state.work_items",
        "projection.agent_state.waiting_intents",
        "projection.agent_state.external_triggers",
        "projection.agent_state.workspace",
        "projection.agent_state.serialization",
        "projection.agent_state.source.loaded",
        "projection.agent_state.source.storage",
        "projection.agent_state.runtime_spawn_avoided",
    ] {
        assert!(
            snapshot
                .projections
                .iter()
                .any(|metric| metric.name == name && metric.count >= 1),
            "missing projection metric {name}"
        );
    }
}
