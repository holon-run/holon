use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod attribution;

const HISTOGRAM_UPPER_BOUNDS_MS: [u64; 18] = [
    0,
    1,
    2,
    4,
    8,
    16,
    32,
    64,
    128,
    256,
    512,
    1_000,
    2_000,
    5_000,
    10_000,
    30_000,
    60_000,
    u64::MAX,
];

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
static DB_SIDECAR_CONSISTENCY_SCAN: MetricAccumulator =
    MetricAccumulator::new("db.sidecar_consistency_scan");

static SCHEDULER_POLL_ALL: MetricAccumulator = MetricAccumulator::new("scheduler.poll.all");
static SCHEDULER_POLL_MESSAGE: MetricAccumulator = MetricAccumulator::new("scheduler.poll.message");
static SCHEDULER_POLL_IDLE: MetricAccumulator = MetricAccumulator::new("scheduler.poll.idle");
static SCHEDULER_POLL_STOPPED: MetricAccumulator = MetricAccumulator::new("scheduler.poll.stopped");
static SCHEDULER_POLL_SHUTDOWN: MetricAccumulator =
    MetricAccumulator::new("scheduler.poll.shutdown");
static SCHEDULER_POLL_SKIPPED: MetricAccumulator = MetricAccumulator::new("scheduler.poll.skipped");
static SCHEDULER_MISSING_TERMINAL_TURN: MetricAccumulator =
    MetricAccumulator::new("scheduler.missing_terminal_turn_detected");
static SCHEDULER_UNSETTLED_CLAIM_RECOVERY: MetricAccumulator =
    MetricAccumulator::new("scheduler.unsettled_claim_recovery");
static SCHEDULER_POISON_MESSAGE_QUARANTINED: MetricAccumulator =
    MetricAccumulator::new("scheduler.poison_message_quarantined");

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
static ROSTER_SNAPSHOT_ASSEMBLY: MetricAccumulator =
    MetricAccumulator::new("observer_sync.roster_snapshot.assembly");
static ROSTER_SNAPSHOT_MEMBER_ROWS: AtomicU64 = AtomicU64::new(0);
static ROSTER_SNAPSHOT_FAILURES: AtomicU64 = AtomicU64::new(0);
static PROJECTION_SNAPSHOT_ASSEMBLY: MetricAccumulator =
    MetricAccumulator::new("observer_sync.projection_snapshot.assembly");
static PROJECTION_SNAPSHOT_FAILURES: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_LEADERS: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_JOINED_WAITERS: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_REJECTED: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_FAILED: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_CANCELLED: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_ACTIVE_PERMITS: AtomicU64 = AtomicU64::new(0);
static PROJECTION_GATE_MAX_ACTIVE_PERMITS: AtomicU64 = AtomicU64::new(0);

static CONVERSATION_SUMMARY: MetricAccumulator = MetricAccumulator::new("conversation.summary");
static CONVERSATION_ACTIVITY: MetricAccumulator = MetricAccumulator::new("conversation.activity");
static CONVERSATION_STREAM_RECOVERY: MetricAccumulator =
    MetricAccumulator::new("conversation.stream_recovery");
static CONVERSATION_SHADOW: MetricAccumulator = MetricAccumulator::new("conversation.shadow");
static CONVERSATION_CAPABILITY_UNAVAILABLE: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_CURSOR_FAILURES: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_LIMIT_FAILURES: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_PAYLOAD_FAILURES: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_SLOW_CONSUMERS: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_LEGACY_UNATTRIBUTED_BRIEFS: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_SHADOW_MATCHES: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_SHADOW_MISMATCHES: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_RETENTION_EXPIRED: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_CURSOR_AHEAD: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_REPLAY_LIMIT_EXCEEDED: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_AGENT_NOT_FOUND: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_QUERY_VERSION_MISMATCH: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_SCHEMA_VERSION_MISMATCH: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_EVENT_LOG_EPOCH_MISMATCH: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_VISIBILITY_SCOPE_MISMATCH: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_STREAM_RECOVERY_FAILED: AtomicU64 = AtomicU64::new(0);
static CONVERSATION_RESET_SLOW_CONSUMER: AtomicU64 = AtomicU64::new(0);

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
    #[serde(default)]
    pub conversation: ConversationDiagnosticsSnapshot,
    #[serde(default)]
    pub diagnostics_writer: crate::diagnostics_store::DiagnosticsWriterStats,
    #[serde(default)]
    pub attribution: Vec<attribution::StageSnapshot>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConversationDiagnosticsSnapshot {
    pub queries: Vec<MetricSnapshot>,
    pub capability_unavailable: u64,
    pub cursor_failures: u64,
    pub limit_failures: u64,
    pub payload_failures: u64,
    pub timeouts: u64,
    pub slow_consumers: u64,
    pub legacy_unattributed_briefs: u64,
    pub shadow_matches: u64,
    pub shadow_mismatches: u64,
    pub resets: ConversationResetDiagnosticsSnapshot,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConversationResetDiagnosticsSnapshot {
    pub retention_expired: u64,
    pub cursor_ahead: u64,
    pub replay_limit_exceeded: u64,
    pub agent_not_found: u64,
    pub query_version_mismatch: u64,
    pub schema_version_mismatch: u64,
    pub event_log_epoch_mismatch: u64,
    pub visibility_scope_mismatch: u64,
    pub stream_recovery_failed: u64,
    pub slow_consumer: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationResetMetricReason {
    RetentionExpired,
    CursorAhead,
    ReplayLimitExceeded,
    AgentNotFound,
    QueryVersionMismatch,
    SchemaVersionMismatch,
    EventLogEpochMismatch,
    VisibilityScopeMismatch,
    StreamRecoveryFailed,
    SlowConsumer,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MetricSnapshot {
    pub name: String,
    pub count: u64,
    pub total_ms: u64,
    pub max_ms: u64,
    pub avg_ms: f64,
    #[serde(default)]
    pub p50_ms: u64,
    #[serde(default)]
    pub p95_ms: u64,
    #[serde(default)]
    pub p99_ms: u64,
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
    histogram: [AtomicU64; HISTOGRAM_UPPER_BOUNDS_MS.len()],
}

impl MetricAccumulator {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            count: AtomicU64::new(0),
            total_ms: AtomicU64::new(0),
            max_ms: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            histogram: [const { AtomicU64::new(0) }; HISTOGRAM_UPPER_BOUNDS_MS.len()],
        }
    }

    fn record(&self, elapsed: Duration, bytes: Option<usize>) {
        let elapsed_ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ms.fetch_add(elapsed_ms, Ordering::Relaxed);
        let bucket =
            HISTOGRAM_UPPER_BOUNDS_MS.partition_point(|upper_bound| *upper_bound < elapsed_ms);
        self.histogram[bucket].fetch_add(1, Ordering::Relaxed);
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

    fn snapshot(&self, include_bytes: bool) -> MetricSnapshot {
        let count = self.count.load(Ordering::Relaxed);
        let total_ms = self.total_ms.load(Ordering::Relaxed);
        let total_bytes = self.total_bytes.load(Ordering::Relaxed);
        let histogram = std::array::from_fn(|index| self.histogram[index].load(Ordering::Relaxed));
        MetricSnapshot {
            name: self.name.to_string(),
            count,
            total_ms,
            max_ms: self.max_ms.load(Ordering::Relaxed),
            avg_ms: average(total_ms, count),
            p50_ms: histogram_quantile(&histogram, 50),
            p95_ms: histogram_quantile(&histogram, 95),
            p99_ms: histogram_quantile(&histogram, 99),
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

pub fn record_conversation_summary(elapsed: Duration, bytes: usize) {
    process_started_at();
    CONVERSATION_SUMMARY.record(elapsed, Some(bytes));
}

pub fn record_conversation_activity(elapsed: Duration, bytes: usize) {
    process_started_at();
    CONVERSATION_ACTIVITY.record(elapsed, Some(bytes));
}

pub fn record_conversation_stream_recovery(elapsed: Duration) {
    process_started_at();
    CONVERSATION_STREAM_RECOVERY.record(elapsed, None);
}

pub fn record_conversation_shadow(
    elapsed: Duration,
    mismatch_count: usize,
    legacy_unattributed_briefs: usize,
) {
    process_started_at();
    CONVERSATION_SHADOW.record(elapsed, None);
    CONVERSATION_LEGACY_UNATTRIBUTED_BRIEFS
        .fetch_add(legacy_unattributed_briefs as u64, Ordering::Relaxed);
    if mismatch_count == 0 {
        CONVERSATION_SHADOW_MATCHES.fetch_add(1, Ordering::Relaxed);
    } else {
        CONVERSATION_SHADOW_MISMATCHES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_conversation_capability_unavailable() {
    process_started_at();
    CONVERSATION_CAPABILITY_UNAVAILABLE.fetch_add(1, Ordering::Relaxed);
}

pub fn record_conversation_cursor_failure() {
    process_started_at();
    CONVERSATION_CURSOR_FAILURES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_conversation_limit_failure() {
    process_started_at();
    CONVERSATION_LIMIT_FAILURES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_conversation_payload_failure() {
    process_started_at();
    CONVERSATION_PAYLOAD_FAILURES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_conversation_timeout() {
    process_started_at();
    CONVERSATION_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_conversation_slow_consumer() {
    process_started_at();
    CONVERSATION_SLOW_CONSUMERS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_conversation_reset(reason: ConversationResetMetricReason) {
    process_started_at();
    let counter = match reason {
        ConversationResetMetricReason::RetentionExpired => &CONVERSATION_RESET_RETENTION_EXPIRED,
        ConversationResetMetricReason::CursorAhead => &CONVERSATION_RESET_CURSOR_AHEAD,
        ConversationResetMetricReason::ReplayLimitExceeded => {
            &CONVERSATION_RESET_REPLAY_LIMIT_EXCEEDED
        }
        ConversationResetMetricReason::AgentNotFound => &CONVERSATION_RESET_AGENT_NOT_FOUND,
        ConversationResetMetricReason::QueryVersionMismatch => {
            &CONVERSATION_RESET_QUERY_VERSION_MISMATCH
        }
        ConversationResetMetricReason::SchemaVersionMismatch => {
            &CONVERSATION_RESET_SCHEMA_VERSION_MISMATCH
        }
        ConversationResetMetricReason::EventLogEpochMismatch => {
            &CONVERSATION_RESET_EVENT_LOG_EPOCH_MISMATCH
        }
        ConversationResetMetricReason::VisibilityScopeMismatch => {
            &CONVERSATION_RESET_VISIBILITY_SCOPE_MISMATCH
        }
        ConversationResetMetricReason::StreamRecoveryFailed => {
            &CONVERSATION_RESET_STREAM_RECOVERY_FAILED
        }
        ConversationResetMetricReason::SlowConsumer => &CONVERSATION_RESET_SLOW_CONSUMER,
    };
    counter.fetch_add(1, Ordering::Relaxed);
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

pub fn record_runtime_db_sidecar_consistency_scan(elapsed: Duration) {
    process_started_at();
    DB_SIDECAR_CONSISTENCY_SCAN.record(elapsed, None);
}

pub fn record_scheduler_poll(outcome: &'static str, elapsed: Duration) {
    process_started_at();
    SCHEDULER_POLL_ALL.record(elapsed, None);
    scheduler_poll_accumulator(outcome).record(elapsed, None);
}

pub fn record_missing_terminal_turn_detected() {
    process_started_at();
    SCHEDULER_MISSING_TERMINAL_TURN.record(Duration::ZERO, None);
}

pub fn record_unsettled_claim_recovery() {
    process_started_at();
    SCHEDULER_UNSETTLED_CLAIM_RECOVERY.record(Duration::ZERO, None);
}

pub fn record_poison_message_quarantined() {
    process_started_at();
    SCHEDULER_POISON_MESSAGE_QUARANTINED.record(Duration::ZERO, None);
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

/// Records one successful roster snapshot assembly: wall-clock duration of
/// the committed read view plus per-Agent entry assembly, the membership
/// count, and the serialized response size.
pub fn record_roster_snapshot(elapsed: Duration, agent_count: usize, bytes: usize) {
    process_started_at();
    ROSTER_SNAPSHOT_ASSEMBLY.record(elapsed, Some(bytes));
    ROSTER_SNAPSHOT_MEMBER_ROWS
        .fetch_add(agent_count.min(u64::MAX as usize) as u64, Ordering::Relaxed);
}

/// Records one roster snapshot request that ended without a response body:
/// capability off, limit exceeded, timeout, or an all-or-nothing assembly
/// failure. Counts only; agent identities are never recorded.
pub fn record_roster_snapshot_failure() {
    process_started_at();
    ROSTER_SNAPSHOT_FAILURES.fetch_add(1, Ordering::Relaxed);
}

/// Records one successful per-Agent projection snapshot assembly:
/// wall-clock duration of the committed read view plus assembly, and the
/// serialized response size.
pub fn record_projection_snapshot(elapsed: Duration, bytes: usize) {
    process_started_at();
    PROJECTION_SNAPSHOT_ASSEMBLY.record(elapsed, Some(bytes));
}

/// Records one projection snapshot request that ended without a response
/// body: capability off, not-found, limit exceeded, timeout, or an
/// all-or-nothing assembly failure. Counts only; agent identities are
/// never recorded.
pub fn record_projection_snapshot_failure() {
    process_started_at();
    PROJECTION_SNAPSHOT_FAILURES.fetch_add(1, Ordering::Relaxed);
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
        attribution: attribution::snapshot(),
        diagnostics_writer: crate::diagnostics_store::writer_stats(),
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
        db: vec![
            DB_CONNECTION_OPEN.snapshot(false),
            DB_SIDECAR_CONSISTENCY_SCAN.snapshot(false),
        ],
        scheduler: vec![
            SCHEDULER_POLL_ALL.snapshot(false),
            SCHEDULER_POLL_MESSAGE.snapshot(false),
            SCHEDULER_POLL_IDLE.snapshot(false),
            SCHEDULER_POLL_STOPPED.snapshot(false),
            SCHEDULER_POLL_SHUTDOWN.snapshot(false),
            SCHEDULER_POLL_SKIPPED.snapshot(false),
            SCHEDULER_MISSING_TERMINAL_TURN.snapshot(false),
            SCHEDULER_UNSETTLED_CLAIM_RECOVERY.snapshot(false),
            SCHEDULER_POISON_MESSAGE_QUARANTINED.snapshot(false),
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
        conversation: ConversationDiagnosticsSnapshot {
            queries: vec![
                CONVERSATION_SUMMARY.snapshot(true),
                CONVERSATION_ACTIVITY.snapshot(true),
                CONVERSATION_STREAM_RECOVERY.snapshot(false),
                CONVERSATION_SHADOW.snapshot(false),
            ],
            capability_unavailable: CONVERSATION_CAPABILITY_UNAVAILABLE.load(Ordering::Relaxed),
            cursor_failures: CONVERSATION_CURSOR_FAILURES.load(Ordering::Relaxed),
            limit_failures: CONVERSATION_LIMIT_FAILURES.load(Ordering::Relaxed),
            payload_failures: CONVERSATION_PAYLOAD_FAILURES.load(Ordering::Relaxed),
            timeouts: CONVERSATION_TIMEOUTS.load(Ordering::Relaxed),
            slow_consumers: CONVERSATION_SLOW_CONSUMERS.load(Ordering::Relaxed),
            legacy_unattributed_briefs: CONVERSATION_LEGACY_UNATTRIBUTED_BRIEFS
                .load(Ordering::Relaxed),
            shadow_matches: CONVERSATION_SHADOW_MATCHES.load(Ordering::Relaxed),
            shadow_mismatches: CONVERSATION_SHADOW_MISMATCHES.load(Ordering::Relaxed),
            resets: ConversationResetDiagnosticsSnapshot {
                retention_expired: CONVERSATION_RESET_RETENTION_EXPIRED.load(Ordering::Relaxed),
                cursor_ahead: CONVERSATION_RESET_CURSOR_AHEAD.load(Ordering::Relaxed),
                replay_limit_exceeded: CONVERSATION_RESET_REPLAY_LIMIT_EXCEEDED
                    .load(Ordering::Relaxed),
                agent_not_found: CONVERSATION_RESET_AGENT_NOT_FOUND.load(Ordering::Relaxed),
                query_version_mismatch: CONVERSATION_RESET_QUERY_VERSION_MISMATCH
                    .load(Ordering::Relaxed),
                schema_version_mismatch: CONVERSATION_RESET_SCHEMA_VERSION_MISMATCH
                    .load(Ordering::Relaxed),
                event_log_epoch_mismatch: CONVERSATION_RESET_EVENT_LOG_EPOCH_MISMATCH
                    .load(Ordering::Relaxed),
                visibility_scope_mismatch: CONVERSATION_RESET_VISIBILITY_SCOPE_MISMATCH
                    .load(Ordering::Relaxed),
                stream_recovery_failed: CONVERSATION_RESET_STREAM_RECOVERY_FAILED
                    .load(Ordering::Relaxed),
                slow_consumer: CONVERSATION_RESET_SLOW_CONSUMER.load(Ordering::Relaxed),
            },
        },
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

fn histogram_quantile(histogram: &[u64; HISTOGRAM_UPPER_BOUNDS_MS.len()], percentile: u64) -> u64 {
    let count = histogram.iter().sum::<u64>();
    if count == 0 {
        return 0;
    }
    let rank = count
        .saturating_mul(percentile)
        .saturating_add(99)
        .saturating_div(100)
        .max(1);
    let mut cumulative = 0_u64;
    for (index, bucket_count) in histogram.iter().enumerate() {
        cumulative = cumulative.saturating_add(*bucket_count);
        if cumulative >= rank {
            return HISTOGRAM_UPPER_BOUNDS_MS[index];
        }
    }
    u64::MAX
}

#[cfg(test)]
mod tests {
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
        assert_eq!(snapshot.p95_ms, u64::MAX);
        assert_eq!(snapshot.p99_ms, u64::MAX);
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
