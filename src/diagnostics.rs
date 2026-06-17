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
static PROJECTION_STATE_TIMERS: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.timers");
static PROJECTION_STATE_WORK_ITEMS: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.work_items");
static PROJECTION_STATE_WAITING: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.waiting_intents");
static PROJECTION_STATE_TRIGGERS: MetricAccumulator =
    MetricAccumulator::new("projection.agent_state.external_triggers");
static PROJECTION_AGENTS_LIST: MetricAccumulator = MetricAccumulator::new("projection.agents_list");

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerformanceDiagnosticsSnapshot {
    pub captured_at: String,
    pub process_uptime_ms: u64,
    pub http: Vec<MetricSnapshot>,
    pub projections: Vec<MetricSnapshot>,
    pub db: Vec<MetricSnapshot>,
    pub scheduler: Vec<MetricSnapshot>,
    pub turn: Vec<MetricSnapshot>,
    pub provider: Vec<MetricSnapshot>,
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

pub fn record_projection_agents_list(elapsed: Duration) {
    process_started_at();
    PROJECTION_AGENTS_LIST.record(elapsed, None);
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
        ],
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
        record_projection_state_tasks(Duration::from_millis(3));
        record_projection_state_timers(Duration::from_millis(1));
        record_projection_state_work_items(Duration::from_millis(2));
        record_projection_state_waiting_intents(Duration::from_millis(1));
        record_projection_state_external_triggers(Duration::from_millis(1));
        record_projection_agents_list(Duration::from_millis(10));

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
    }
}
