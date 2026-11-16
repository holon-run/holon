use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerformanceDiagnosticsSnapshot {
    pub captured_at: String,
    pub process_uptime_ms: u64,
    pub http: Vec<MetricSnapshot>,
    pub projections: Vec<MetricSnapshot>,
    pub projection_gate: ProjectionGateDiagnosticsSnapshot,
    pub db: Vec<MetricSnapshot>,
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
