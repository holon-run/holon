use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

static PROCESS_STARTED_AT: OnceLock<Instant> = OnceLock::new();

static HTTP_ALL: MetricAccumulator = MetricAccumulator::new("http.json.all");
static HTTP_MODELS: MetricAccumulator = MetricAccumulator::new("http.json./models");
static HTTP_AGENTS_LIST: MetricAccumulator = MetricAccumulator::new("http.json./agents/list");
static HTTP_AGENT_STATUS: MetricAccumulator =
    MetricAccumulator::new("http.json./agents/{agent_id}/status");
static HTTP_AGENT_STATE: MetricAccumulator =
    MetricAccumulator::new("http.json./agents/{agent_id}/state");
static HTTP_OTHER: MetricAccumulator = MetricAccumulator::new("http.json.other");

static PROJECTION_AGENT_SUMMARY: MetricAccumulator =
    MetricAccumulator::new("projection.agent_summary");
static PROJECTION_RUNTIME_CACHE_REBUILD: MetricAccumulator =
    MetricAccumulator::new("projection.runtime_current_cache.rebuild");
static PROJECTION_RUNTIME_CACHE_READ: MetricAccumulator =
    MetricAccumulator::new("projection.runtime_current_cache.read");
static OBJECT_QUERY_CACHE_HIT: MetricAccumulator = MetricAccumulator::new("object_query_cache.hit");
static OBJECT_QUERY_CACHE_MISS: MetricAccumulator =
    MetricAccumulator::new("object_query_cache.miss");
static DB_CONNECTION_OPEN: MetricAccumulator = MetricAccumulator::new("db.connection.open");

static SCHEDULER_POLL_ALL: MetricAccumulator = MetricAccumulator::new("scheduler.poll.all");
static SCHEDULER_POLL_MESSAGE: MetricAccumulator = MetricAccumulator::new("scheduler.poll.message");
static SCHEDULER_POLL_IDLE: MetricAccumulator = MetricAccumulator::new("scheduler.poll.idle");
static SCHEDULER_POLL_STOPPED: MetricAccumulator = MetricAccumulator::new("scheduler.poll.stopped");
static SCHEDULER_POLL_SHUTDOWN: MetricAccumulator =
    MetricAccumulator::new("scheduler.poll.shutdown");
static SCHEDULER_POLL_SKIPPED: MetricAccumulator = MetricAccumulator::new("scheduler.poll.skipped");

// Turn lifecycle
static TURN_TOTAL: MetricAccumulator = MetricAccumulator::new("turn.total");
static TURN_CONTEXT_BUILD: MetricAccumulator = MetricAccumulator::new("turn.context_build");
static TURN_PROVIDER_ROUND: MetricAccumulator = MetricAccumulator::new("turn.provider_round");
static TURN_TOOL_EXECUTION: MetricAccumulator = MetricAccumulator::new("turn.tool_execution");
static TURN_CLEANUP: MetricAccumulator = MetricAccumulator::new("turn.cleanup");

// Provider phases
static PROVIDER_REQUEST_BUILD: MetricAccumulator = MetricAccumulator::new("provider.request_build");
static PROVIDER_ROUND_TOTAL: MetricAccumulator = MetricAccumulator::new("provider.round_total");
static PROVIDER_RETRY: MetricAccumulator = MetricAccumulator::new("provider.retry");

// Tool phase
static TOOL_EXECUTION: MetricAccumulator = MetricAccumulator::new("tool.execution");

// Persistence
static STORAGE_APPEND_EVENT: MetricAccumulator = MetricAccumulator::new("storage.append_event");
static STORAGE_PERSIST_STATE: MetricAccumulator = MetricAccumulator::new("storage.persist_state");

// Projection/API substeps
static PROJECTION_STATE_TASKS: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.tasks");
static PROJECTION_STATE_AGENT: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.agent");
static PROJECTION_STATE_TIMERS: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.timers");
static PROJECTION_STATE_WORK_ITEMS: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.work_items");
static PROJECTION_STATE_WAITING: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.waiting_intents");
static PROJECTION_STATE_TRIGGERS: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.external_triggers");
static PROJECTION_STATE_WORKSPACE: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.workspace");
static PROJECTION_STATE_SERIALIZATION: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.serialization");
static PROJECTION_STATE_SOURCE_LOADED: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.source.loaded");
static PROJECTION_STATE_SOURCE_STORAGE: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.source.storage");
static PROJECTION_STATE_RUNTIME_SPAWN_AVOIDED: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.runtime_spawn_avoided");
static PROJECTION_AGENTS_LIST: MetricAccumulator = MetricAccumulator::new("projection.agents_list");
static PROJECTION_GATE_LEADERS: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_JOINED_WAITERS: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_REJECTED: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_FAILED: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_CANCELLED: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_ACTIVE_PERMITS: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_MAX_ACTIVE_PERMITS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerformanceDiagnosticsSnapshot {
    pub captured_at: String,
    pub process_uptime_ms: u64,
    pub http: Vec<MetricSnapshot>,
    pub projections: Vec<MetricSnapshot>,
    pub projection_gate: ProjectionGateDiagnosticsSnapshot,
    pub db: Vec<MetricSnapshot>,
    pub scheduler: Vec<MetricSnapshot>,
    pub turn: Vec<MetricSnapshot>,
    pub provider: Vec<MetricSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectionGateDiagnosticsSnapshot {
    pub leaders: u64,
    pub joined_waiters: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub rejected: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub active_permits: u64,
    pub max_active_permits: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MetricSnapshot {
    pub name: String,
    pub count: u64,
    pub total_ms: u64,
    pub max_ms: u64,
    pub avg_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_bytes: Option<f64>,
}

struct MetricAccumulator {
    name: &'static str,
    count: AtomicU64,
    total_ms: AtomicU64,
    max_ms: AtomicU64,
    total_bytes: AtomicU64,
}

impl MetricAccumulator {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            count: AtomicU64::new(0),
            total_ms: AtomicU64::new(0),
            max_ms: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
        }
    }

    fn record(&self, elapsed: Duration, bytes: Option<usize>) {
        let elapsed_ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ms.fetch_add(elapsed_ms, Ordering::Relaxed);
        if let Some(bytes) = bytes {
            self.total_bytes
                .fetch_add(bytes.min(u64::MAX as usize) as u64, Ordering::Relaxed);
        }
        let mut current = self.max_ms.load(Ordering::Relaxed);
        while elapsed_ms > current {
            match self.max_ms.compare_exchange_weak(
                current,
                elapsed_ms,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => current = next,
            }
        }
    }

    fn snapshot(&'static self, include_bytes: bool) -> MetricSnapshot {
        let count = self.count.load(Ordering::Relaxed);
        let total_ms = self.total_ms.load(Ordering::Relaxed);
        let total_bytes = self.total_bytes.load(Ordering::Relaxed);
        MetricSnapshot {
            name: self.name.to_string(),
            count,
            total_ms,
            max_ms: self.max_ms.load(Ordering::Relaxed),
            avg_ms: average(total_ms, count),
            total_bytes: include_bytes.then_some(total_bytes),
            avg_bytes: include_bytes.then_some(average(total_bytes, count)),
        }
    }
}

pub fn record_http_json_response(route: &'static str, elapsed: Duration, bytes: usize) {
    process_started_at();
    HTTP_ALL.record(elapsed, Some(bytes));
    http_route_accumulator(route).record(elapsed, Some(bytes));
}

pub fn record_agent_summary_projection(elapsed: Duration) {
    process_started_at();
    PROJECTION_AGENT_SUMMARY.record(elapsed, None);
}

pub fn record_runtime_projection_cache_rebuild() {
    process_started_at();
    PROJECTION_RUNTIME_CACHE_REBUILD.record(Duration::ZERO, None);
}

pub fn record_runtime_projection_cache_read() {
    process_started_at();
    PROJECTION_RUNTIME_CACHE_READ.record(Duration::ZERO, None);
}

pub fn record_object_query_cache_hit() {
    process_started_at();
    OBJECT_QUERY_CACHE_HIT.record(Duration::ZERO, None);
}

pub fn record_object_query_cache_miss() {
    process_started_at();
    OBJECT_QUERY_CACHE_MISS.record(Duration::ZERO, None);
}

pub fn record_runtime_db_connection_open(elapsed: Duration) {
    process_started_at();
    DB_CONNECTION_OPEN.record(elapsed, None);
}

pub fn record_scheduler_poll(outcome: &'static str, elapsed: Duration) {
    process_started_at();
    SCHEDULER_POLL_ALL.record(elapsed, None);
    scheduler_poll_accumulator(outcome).record(elapsed, None);
}

// Turn lifecycle recording

pub fn record_turn_total(elapsed: Duration) {
    process_started_at();
    TURN_TOTAL.record(elapsed, None);
}

pub fn record_turn_context_build(elapsed: Duration) {
    process_started_at();
    TURN_CONTEXT_BUILD.record(elapsed, None);
}

pub fn record_turn_provider_round(elapsed: Duration) {
    process_started_at();
    TURN_PROVIDER_ROUND.record(elapsed, None);
}

pub fn record_turn_tool_execution(elapsed: Duration) {
    process_started_at();
    TURN_TOOL_EXECUTION.record(elapsed, None);
}

pub fn record_turn_cleanup(elapsed: Duration) {
    process_started_at();
    TURN_CLEANUP.record(elapsed, None);
}

// Provider phase recording

pub fn record_provider_request_build(elapsed: Duration) {
    process_started_at();
    PROVIDER_REQUEST_BUILD.record(elapsed, None);
}

pub fn record_provider_round_total(elapsed: Duration) {
    process_started_at();
    PROVIDER_ROUND_TOTAL.record(elapsed, None);
}

pub fn record_provider_retry(elapsed: Duration) {
    process_started_at();
    PROVIDER_RETRY.record(elapsed, None);
}

// Tool execution recording

pub fn record_tool_execution(_tool_name: &str, elapsed: Duration, output_bytes: Option<usize>) {
    process_started_at();
    TOOL_EXECUTION.record(elapsed, output_bytes);
}

// Persistence recording

pub fn record_storage_append_event(elapsed: Duration) {
    process_started_at();
    STORAGE_APPEND_EVENT.record(elapsed, None);
}

pub fn record_storage_persist_state(elapsed: Duration) {
    process_started_at();
    STORAGE_PERSIST_STATE.record(elapsed, None);
}

// Projection substep recording

pub fn record_projection_state_tasks(elapsed: Duration) {
    process_started_at();
    PROJECTION_STATE_TASKS.record(elapsed, None);
}

pub fn record_projection_state_agent(elapsed: Duration) {
    process_started_at();
    PROJECTION_STATE_AGENT.record(elapsed, None);
}

pub fn record_projection_state_timers(elapsed: Duration) {
    process_started_at();
    PROJECTION_STATE_TIMERS.record(elapsed, None);
}

pub fn record_projection_state_work_items(elapsed: Duration) {
    process_started_at();
    PROJECTION_STATE_WORK_ITEMS.record(elapsed, None);
}

pub fn record_projection_state_waiting_intents(elapsed: Duration) {
    process_started_at();
    PROJECTION_STATE_WAITING.record(elapsed, None);
}

pub fn record_projection_state_external_triggers(elapsed: Duration) {
    process_started_at();
    PROJECTION_STATE_TRIGGERS.record(elapsed, None);
}

pub fn record_projection_state_workspace(elapsed: Duration) {
    process_started_at();
    PROJECTION_STATE_WORKSPACE.record(elapsed, None);
}

pub fn record_projection_state_serialization(elapsed: Duration) {
    process_started_at();
    PROJECTION_STATE_SERIALIZATION.record(elapsed, None);
}

pub fn record_projection_state_source_loaded() {
    process_started_at();
    PROJECTION_STATE_SOURCE_LOADED.record(Duration::ZERO, None);
}

pub fn record_projection_state_source_storage() {
    process_started_at();
    PROJECTION_STATE_SOURCE_STORAGE.record(Duration::ZERO, None);
}

pub fn record_projection_state_runtime_spawn_avoided() {
    process_started_at();
    PROJECTION_STATE_RUNTIME_SPAWN_AVOIDED.record(Duration::ZERO, None);
}

pub fn record_projection_agents_list(elapsed: Duration) {
    process_started_at();
    PROJECTION_AGENTS_LIST.record(elapsed, None);
}

pub fn record_projection_gate_cache_hit() {
    process_started_at();
    PROJECTION_GATE_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_projection_gate_cache_miss() {
    process_started_at();
    PROJECTION_GATE_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_projection_gate_joined_waiter() {
    process_started_at();
    PROJECTION_GATE_JOINED_WAITERS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_projection_gate_rejected() {
    process_started_at();
    PROJECTION_GATE_REJECTED.fetch_add(1, Ordering::Relaxed);
}

pub fn record_projection_gate_failed() {
    process_started_at();
    PROJECTION_GATE_FAILED.fetch_add(1, Ordering::Relaxed);
}

pub fn record_projection_gate_cancelled() {
    process_started_at();
    PROJECTION_GATE_CANCELLED.fetch_add(1, Ordering::Relaxed);
}

pub fn record_projection_gate_leader_started() {
    process_started_at();
    PROJECTION_GATE_LEADERS.fetch_add(1, Ordering::Relaxed);
    let active = PROJECTION_GATE_ACTIVE_PERMITS.fetch_add(1, Ordering::Relaxed) + 1;
    update_max(&PROJECTION_GATE_MAX_ACTIVE_PERMITS, active);
}

pub fn record_projection_gate_leader_finished() {
    let mut current = PROJECTION_GATE_ACTIVE_PERMITS.load(Ordering::Relaxed);
    while current > 0 {
        match PROJECTION_GATE_ACTIVE_PERMITS.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

pub fn performance_snapshot() -> PerformanceDiagnosticsSnapshot {
    let started_at = process_started_at();
    PerformanceDiagnosticsSnapshot {
        captured_at: Utc::now().to_rfc3339(),
        process_uptime_ms: started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        http: vec![
            HTTP_ALL.snapshot(true),
            HTTP_MODELS.snapshot(true),
            HTTP_AGENTS_LIST.snapshot(true),
            HTTP_AGENT_STATUS.snapshot(true),
            HTTP_AGENT_STATE.snapshot(true),
            HTTP_OTHER.snapshot(true),
        ],
        projections: vec![
            PROJECTION_AGENT_SUMMARY.snapshot(false),
            PROJECTION_RUNTIME_CACHE_REBUILD.snapshot(false),
            PROJECTION_RUNTIME_CACHE_READ.snapshot(false),
            OBJECT_QUERY_CACHE_HIT.snapshot(false),
            OBJECT_QUERY_CACHE_MISS.snapshot(false),
            PROJECTION_AGENTS_LIST.snapshot(false),
            PROJECTION_STATE_AGENT.snapshot(false),
            PROJECTION_STATE_TASKS.snapshot(false),
            PROJECTION_STATE_TIMERS.snapshot(false),
            PROJECTION_STATE_WORK_ITEMS.snapshot(false),
            PROJECTION_STATE_WAITING.snapshot(false),
            PROJECTION_STATE_TRIGGERS.snapshot(false),
            PROJECTION_STATE_WORKSPACE.snapshot(false),
            PROJECTION_STATE_SERIALIZATION.snapshot(false),
            PROJECTION_STATE_SOURCE_LOADED.snapshot(false),
            PROJECTION_STATE_SOURCE_STORAGE.snapshot(false),
            PROJECTION_STATE_RUNTIME_SPAWN_AVOIDED.snapshot(false),
        ],
        projection_gate: ProjectionGateDiagnosticsSnapshot {
            leaders: PROJECTION_GATE_LEADERS.load(Ordering::Relaxed),
            joined_waiters: PROJECTION_GATE_JOINED_WAITERS.load(Ordering::Relaxed),
            cache_hits: PROJECTION_GATE_CACHE_HITS.load(Ordering::Relaxed),
            cache_misses: PROJECTION_GATE_CACHE_MISSES.load(Ordering::Relaxed),
            rejected: PROJECTION_GATE_REJECTED.load(Ordering::Relaxed),
            failed: PROJECTION_GATE_FAILED.load(Ordering::Relaxed),
            cancelled: PROJECTION_GATE_CANCELLED.load(Ordering::Relaxed),
            active_permits: PROJECTION_GATE_ACTIVE_PERMITS.load(Ordering::Relaxed),
            max_active_permits: PROJECTION_GATE_MAX_ACTIVE_PERMITS.load(Ordering::Relaxed),
        },
        db: vec![DB_CONNECTION_OPEN.snapshot(false)],
        scheduler: vec![
            SCHEDULER_POLL_ALL.snapshot(false),
            SCHEDULER_POLL_MESSAGE.snapshot(false),
            SCHEDULER_POLL_IDLE.snapshot(false),
            SCHEDULER_POLL_STOPPED.snapshot(false),
            SCHEDULER_POLL_SHUTDOWN.snapshot(false),
            SCHEDULER_POLL_SKIPPED.snapshot(false),
        ],
        turn: vec![
            TURN_TOTAL.snapshot(false),
            TURN_CONTEXT_BUILD.snapshot(false),
            TURN_PROVIDER_ROUND.snapshot(false),
            TURN_TOOL_EXECUTION.snapshot(false),
            TURN_CLEANUP.snapshot(false),
        ],
        provider: vec![
            PROVIDER_REQUEST_BUILD.snapshot(false),
            PROVIDER_ROUND_TOTAL.snapshot(false),
            PROVIDER_RETRY.snapshot(false),
        ],
    }
}

fn update_max(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

fn http_route_accumulator(route: &'static str) -> &'static MetricAccumulator {
    match route {
        "/models" => &HTTP_MODELS,
        "/agents/list" => &HTTP_AGENTS_LIST,
        "/agents/{agent_id}/status" => &HTTP_AGENT_STATUS,
        "/agents/{agent_id}/state" => &HTTP_AGENT_STATE,
        _ => &HTTP_OTHER,
    }
}

fn scheduler_poll_accumulator(outcome: &'static str) -> &'static MetricAccumulator {
    match outcome {
        "message" => &SCHEDULER_POLL_MESSAGE,
        "idle" => &SCHEDULER_POLL_IDLE,
        "stopped" => &SCHEDULER_POLL_STOPPED,
        "shutdown" => &SCHEDULER_POLL_SHUTDOWN,
        _ => &SCHEDULER_POLL_SKIPPED,
    }
}

fn process_started_at() -> &'static Instant {
    PROCESS_STARTED_AT.get_or_init(Instant::now)
}

fn average(total: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        total as f64 / count as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_includes_bounded_runtime_hotspot_groups() {
        record_http_json_response("/agents/{agent_id}/state", Duration::from_millis(7), 1024);
        record_agent_summary_projection(Duration::from_millis(3));
        record_runtime_projection_cache_rebuild();
        record_runtime_projection_cache_read();
        record_runtime_db_connection_open(Duration::from_millis(2));
        record_scheduler_poll("idle", Duration::from_millis(1));

        let snapshot = performance_snapshot();

        assert!(
            snapshot
                .http
                .iter()
                .any(|metric| metric.name == "http.json./agents/{agent_id}/state"
                    && metric.count >= 1)
        );
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
}
