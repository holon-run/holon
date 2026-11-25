use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerformanceDiagnosticsSnapshot {
    pub captured_at: String,
    pub process_uptime_ms: u64,
    pub http: Vec<MetricSnapshot>,
    pub projections: Vec<MetricSnapshot>,
    pub projection_gate: ProjectionGateDiagnosticsSnapshot,
    pub db: Vec<MetricSnapshot>,
    /// Process-lifetime reader-pool diagnostics for the `runtime_db` resource
    /// scope, aggregated across every connected owner. Older snapshots omit
    /// this field and deserialize as empty.
    #[serde(default)]
    pub reader_pools: Vec<ReaderPoolDiagnosticsSnapshot>,
    #[serde(default)]
    pub runtime_db_wal: RuntimeDbWalCheckpointDiagnostics,
    #[serde(default)]
    pub memory_index_writer: MemoryIndexWriterDiagnosticsSnapshot,
    pub scheduler: Vec<MetricSnapshot>,
    pub turn: Vec<MetricSnapshot>,
    pub provider: Vec<MetricSnapshot>,
    #[serde(default)]
    pub tools: Vec<MetricSnapshot>,
    #[serde(default)]
    pub conversation: ConversationDiagnosticsSnapshot,
    #[serde(default)]
    pub diagnostics_writer: DiagnosticsWriterStats,
    #[serde(default)]
    pub attribution: Vec<StageSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeDbWalCheckpointDiagnostics {
    /// RFC 3339 timestamp of the last maintenance checkpoint pass; `None`
    /// until the daemon completes its first round.
    #[serde(default)]
    pub last_pass_at: Option<String>,
    pub wal_bytes_before: u64,
    pub wal_bytes_after: u64,
    pub busy: bool,
    pub log_frames: u64,
    pub checkpointed_frames: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MemoryIndexWriterDiagnosticsSnapshot {
    pub queue_depth: u64,
    pub max_queue_depth: u64,
    pub foreground_queue_depth: u64,
    pub max_foreground_queue_depth: u64,
    pub maintenance_queue_depth: u64,
    pub max_maintenance_queue_depth: u64,
    pub foreground_timeouts: u64,
    pub maintenance_timeouts: u64,
    pub operations: Vec<MemoryIndexWriterOperationSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryIndexWriterOperationSnapshot {
    pub operation: String,
    pub queue_wait: MetricSnapshot,
    pub timeouts: u64,
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
    pub stale_served: u64,
    pub stale_expired: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StageSnapshot {
    pub name: String,
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub rows: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticsWriterStats {
    pub queue_depth: u64,
    pub queued_traces: u64,
    pub dropped_traces: u64,
    pub filtered_traces: u64,
    pub persisted_traces: u64,
    pub retention_deleted_traces: u64,
    pub writer_failures: u64,
}

/// Transport-normalized token usage for one provider attempt
/// (`attempt_token_usage.v1`).
///
/// The four flat token fields on [`TraceAttributes`]
/// (`input_tokens`/`output_tokens`/`cache_read_input_tokens`/
/// `cache_creation_input_tokens`) stay as raw per-transport wire evidence and
/// keep their historical (transport-dependent) meaning. This object is the
/// normalized view that downstream cost code can use without branching on
/// transport.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceTokenUsage {
    /// Input billed at full price (cache reads/writes excluded).
    pub uncached_input_tokens: u64,
    /// Cache read hits.
    pub cache_read_tokens: u64,
    /// Cache writes.
    pub cache_write_tokens: u64,
    /// Total billed output, including reasoning / thinking tokens.
    pub output_tokens: u64,
    /// Subset of `output_tokens`; `None` when the transport does not report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Schema version of the normalization contract, currently
    /// `"attempt_token_usage.v1"`.
    pub semantics: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceAttributes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_http_trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TraceTokenUsage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraceSpanStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceSpan {
    pub name: String,
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    pub duration_us: u64,
    pub status: TraceSpanStatus,
    pub attributes: TraceAttributes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RecentTrace {
    pub trace_id: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    pub duration_us: u64,
    pub span_count: usize,
    pub dropped_spans: u64,
    pub error_count: usize,
    pub spans: Vec<TraceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RecentTraceSummary {
    pub trace_id: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    pub duration_us: u64,
    pub span_count: usize,
    pub dropped_spans: u64,
    pub error_count: usize,
}

impl From<&RecentTrace> for RecentTraceSummary {
    fn from(trace: &RecentTrace) -> Self {
        Self {
            trace_id: trace.trace_id.clone(),
            started_at: trace.started_at,
            completed_at: trace.completed_at,
            duration_us: trace.duration_us,
            span_count: trace.span_count,
            dropped_spans: trace.dropped_spans,
            error_count: trace.error_count,
        }
    }
}

/// Fixed reader-pool role dimension.
///
/// Roles are the only permitted role label values in exported metrics;
/// runtime code must not invent additional roles or free-form owner labels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReaderPoolRole {
    #[default]
    Ordinary,
    Preparation,
}

/// Outcome of one finished reader-pool checkout attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderPoolOutcome {
    Success,
    Timeout,
    Cancelled,
    Failed,
}

/// One reader-pool lifecycle observation.
///
/// This is the narrow event contract between the storage adapter and the
/// observability accumulator. It carries no owner identity, path, SQL, or
/// business value, so it cannot create unbounded label cardinality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderPoolEvent {
    /// A shared pool facade was created successfully; `capacity` is its
    /// configured maximum.
    Created { capacity: u32 },
    /// The last facade for a pool was dropped; `capacity` matches `Created`.
    Dropped { capacity: u32 },
    /// A configured connection will be returned to r2d2.
    ConnectionOpened,
    /// SQLite actually closed a connection.
    ConnectionClosed,
    /// A public sync/async read entry started waiting for a lease.
    CheckoutStarted,
    /// A lease was obtained, or the attempt failed/timed out/was cancelled
    /// before that point. `elapsed` covers admission, scheduling, and queueing,
    /// not query execution.
    CheckoutFinished {
        elapsed: Duration,
        outcome: ReaderPoolOutcome,
    },
    /// A checked-out lease became owned by an application job.
    LeaseStarted,
    /// A lease was returned or released after business work, decoding, and
    /// cleanup finished.
    LeaseFinished { elapsed: Duration },
    /// One `manager.connect` attempt started.
    ConnectStarted,
    /// One `manager.connect` attempt finished. Attempts and failures are
    /// counted separately.
    ConnectFinished { elapsed: Duration, success: bool },
}

/// Fixed-bucket latency distribution.
///
/// `buckets` is cumulative: each entry counts observations at or below its
/// upper bound, and the last entry is `+Inf`. Bounds are the shared fixed
/// millisecond bounds, so they never depend on the workload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReaderPoolDurationSnapshot {
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub buckets: Vec<ReaderPoolBucketSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReaderPoolBucketSnapshot {
    /// Inclusive upper bound in milliseconds; `None` denotes `+Inf`.
    pub upper_bound_ms: Option<u64>,
    /// Cumulative observation count at or below `upper_bound_ms`.
    pub count: u64,
}

/// Reader-pool diagnostics for one fixed role within `resource_scope=runtime_db`.
///
/// Gauge-like fields are current lifecycle values and fall back when a pool is
/// dropped. `*_created`/`*_opened`/`*_started`/`*_finished` counters are
/// process-lifetime totals and never decrease. `idle` is derived as live
/// connections minus application lease holders; it is a non-transactional
/// snapshot, so concurrent connect/teardown transitions may show transient
/// combinations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReaderPoolDiagnosticsSnapshot {
    pub role: ReaderPoolRole,
    /// Live pool facades (`Created` minus `Dropped`).
    pub instances: u64,
    /// Sum of live pools' configured capacities.
    pub configured_capacity: u64,
    /// Live configured connections (`ConnectionOpened` minus `ConnectionClosed`).
    pub connections: u64,
    /// Live application lease holders (`LeaseStarted` minus `LeaseFinished`).
    pub in_use: u64,
    /// `connections` minus `in_use`, floored at zero.
    pub idle: u64,
    /// Checkouts that started and have not finished yet. Execution time is not
    /// part of checkout waiting.
    pub waiting: u64,
    pub instances_created: u64,
    pub instances_dropped: u64,
    pub connections_opened: u64,
    pub connections_closed: u64,
    pub checkouts_started: u64,
    pub checkouts_succeeded: u64,
    pub checkouts_timed_out: u64,
    pub checkouts_cancelled: u64,
    pub checkouts_failed: u64,
    pub checkout_duration: ReaderPoolDurationSnapshot,
    pub leases_started: u64,
    pub leases_finished: u64,
    pub lease_duration: ReaderPoolDurationSnapshot,
    pub connects_started: u64,
    pub connects_succeeded: u64,
    pub connects_failed: u64,
    pub connect_duration: ReaderPoolDurationSnapshot,
}

#[cfg(test)]
mod tests;
