use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

const TRACE_ID_HEX_LEN: usize = 32;
const SPAN_ID_HEX_LEN: usize = 16;
const RECENT_TRACE_LIMIT: usize = 128;
const SPANS_PER_TRACE_LIMIT: usize = 256;

static RECENT_TRACES: OnceLock<Mutex<RecentTraceStore>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceContextError {
    InvalidTraceParent,
    UnsupportedVersion,
    InvalidTraceState,
}

impl fmt::Display for TraceContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTraceParent => formatter.write_str("invalid W3C traceparent"),
            Self::UnsupportedVersion => formatter.write_str("unsupported W3C traceparent version"),
            Self::InvalidTraceState => formatter.write_str("invalid W3C tracestate"),
        }
    }
}

impl std::error::Error for TraceContextError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub trace_flags: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_state: Option<String>,
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

#[derive(Debug, Default)]
struct RecentTraceStore {
    traces: VecDeque<RecentTrace>,
}

pub fn record_span(context: &TraceContext, span: TraceSpan) {
    let mut store = recent_traces()
        .lock()
        .expect("recent trace store lock poisoned");
    let mut trace = if let Some(index) = store
        .traces
        .iter()
        .position(|trace| trace.trace_id == context.trace_id)
    {
        store
            .traces
            .remove(index)
            .expect("trace index should remain valid")
    } else {
        RecentTrace {
            trace_id: context.trace_id.clone(),
            started_at: span.started_at,
            completed_at: span.completed_at,
            duration_us: span.duration_us,
            span_count: 0,
            dropped_spans: 0,
            error_count: 0,
            spans: Vec::new(),
        }
    };
    trace.started_at = trace.started_at.min(span.started_at);
    trace.completed_at = trace.completed_at.max(span.completed_at);
    trace.duration_us = elapsed_us(trace.started_at, trace.completed_at);
    trace.span_count = trace.span_count.saturating_add(1);
    if span.status == TraceSpanStatus::Error {
        trace.error_count = trace.error_count.saturating_add(1);
    }
    if trace.spans.len() == SPANS_PER_TRACE_LIMIT {
        trace.spans.remove(0);
        trace.dropped_spans = trace.dropped_spans.saturating_add(1);
    }
    trace.spans.push(span);
    store.traces.push_front(trace);
    store.traces.truncate(RECENT_TRACE_LIMIT);
}

pub fn recent_trace_summaries() -> Vec<RecentTraceSummary> {
    recent_traces()
        .lock()
        .expect("recent trace store lock poisoned")
        .traces
        .iter()
        .map(RecentTraceSummary::from)
        .collect()
}

pub fn recent_trace(trace_id: &str) -> Option<RecentTrace> {
    recent_traces()
        .lock()
        .expect("recent trace store lock poisoned")
        .traces
        .iter()
        .find(|trace| trace.trace_id == trace_id)
        .cloned()
}

pub fn search_recent_traces(query: &str) -> Vec<RecentTraceSummary> {
    recent_traces()
        .lock()
        .expect("recent trace store lock poisoned")
        .traces
        .iter()
        .filter(|trace| {
            trace.trace_id == query
                || trace.spans.iter().any(|span| {
                    [
                        span.attributes.turn_id.as_deref(),
                        span.attributes.message_id.as_deref(),
                        span.attributes.run_id.as_deref(),
                        span.attributes.work_item_id.as_deref(),
                        span.attributes.task_id.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|value| value == query)
                })
        })
        .map(RecentTraceSummary::from)
        .collect()
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

pub fn completed_span(
    name: impl Into<String>,
    context: &TraceContext,
    parent_span_id: Option<String>,
    started_at: chrono::DateTime<chrono::Utc>,
    status: TraceSpanStatus,
    attributes: TraceAttributes,
) -> TraceSpan {
    completed_span_at(
        name,
        context,
        parent_span_id,
        started_at,
        chrono::Utc::now(),
        status,
        attributes,
    )
}

pub fn completed_span_at(
    name: impl Into<String>,
    context: &TraceContext,
    parent_span_id: Option<String>,
    started_at: chrono::DateTime<chrono::Utc>,
    completed_at: chrono::DateTime<chrono::Utc>,
    status: TraceSpanStatus,
    attributes: TraceAttributes,
) -> TraceSpan {
    TraceSpan {
        name: name.into(),
        span_id: context.span_id.clone(),
        parent_span_id,
        started_at,
        completed_at,
        duration_us: elapsed_us(started_at, completed_at),
        status,
        attributes,
    }
}

fn recent_traces() -> &'static Mutex<RecentTraceStore> {
    RECENT_TRACES.get_or_init(|| Mutex::new(RecentTraceStore::default()))
}

fn elapsed_us(
    started_at: chrono::DateTime<chrono::Utc>,
    completed_at: chrono::DateTime<chrono::Utc>,
) -> u64 {
    completed_at
        .signed_duration_since(started_at)
        .num_microseconds()
        .unwrap_or_default()
        .max(0) as u64
}

impl TraceContext {
    pub fn new_root(sampled: bool) -> Self {
        Self {
            trace_id: Uuid::new_v4().simple().to_string(),
            span_id: random_span_id(),
            trace_flags: u8::from(sampled),
            trace_state: None,
        }
    }

    pub fn parse(trace_parent: &str, trace_state: Option<&str>) -> Result<Self, TraceContextError> {
        let mut parts = trace_parent.trim().split('-');
        let version = parts.next().ok_or(TraceContextError::InvalidTraceParent)?;
        let trace_id = parts.next().ok_or(TraceContextError::InvalidTraceParent)?;
        let span_id = parts.next().ok_or(TraceContextError::InvalidTraceParent)?;
        let trace_flags = parts.next().ok_or(TraceContextError::InvalidTraceParent)?;
        if parts.next().is_some() || version.len() != 2 || !is_lower_hex(version) || version == "ff"
        {
            return Err(TraceContextError::InvalidTraceParent);
        }
        if version != "00" {
            return Err(TraceContextError::UnsupportedVersion);
        }
        if !valid_nonzero_hex(trace_id, TRACE_ID_HEX_LEN)
            || !valid_nonzero_hex(span_id, SPAN_ID_HEX_LEN)
            || trace_flags.len() != 2
            || !is_lower_hex(trace_flags)
        {
            return Err(TraceContextError::InvalidTraceParent);
        }
        let trace_flags = u8::from_str_radix(trace_flags, 16)
            .map_err(|_| TraceContextError::InvalidTraceParent)?;
        let trace_state = trace_state
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(validate_trace_state)
            .transpose()?;
        Ok(Self {
            trace_id: trace_id.to_owned(),
            span_id: span_id.to_owned(),
            trace_flags,
            trace_state,
        })
    }

    pub fn trace_parent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id, self.span_id, self.trace_flags
        )
    }

    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: random_span_id(),
            trace_flags: self.trace_flags,
            trace_state: self.trace_state.clone(),
        }
    }

    pub fn sampled(&self) -> bool {
        self.trace_flags & 1 == 1
    }
}

fn random_span_id() -> String {
    Uuid::new_v4().as_bytes()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_nonzero_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && is_lower_hex(value)
        && value.as_bytes().iter().any(|byte| *byte != b'0')
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_trace_state(value: &str) -> Result<String, TraceContextError> {
    if value.len() > 512 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(TraceContextError::InvalidTraceState);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_context_round_trips_w3c_headers() {
        let context = TraceContext::parse(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            Some("vendor=value"),
        )
        .unwrap();

        assert_eq!(
            context.trace_parent(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );
        assert_eq!(context.trace_state.as_deref(), Some("vendor=value"));
        assert!(context.sampled());
    }

    #[test]
    fn child_preserves_trace_and_changes_span() {
        let parent = TraceContext::new_root(true);
        let child = parent.child();

        assert_eq!(child.trace_id, parent.trace_id);
        assert_ne!(child.span_id, parent.span_id);
        assert_eq!(child.trace_flags, parent.trace_flags);
    }

    #[test]
    fn trace_attributes_deserialize_without_phase_two_fields() {
        let attributes: TraceAttributes =
            serde_json::from_str(r#"{"agent_id":"agent-1","round":2}"#).unwrap();

        assert_eq!(attributes.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(attributes.round, Some(2));
        assert_eq!(attributes.attempt, None);
        assert_eq!(attributes.failure_kind, None);
        assert_eq!(attributes.provider_http_trace_id, None);
    }

    #[test]
    fn rejects_invalid_or_unsupported_traceparent() {
        assert_eq!(
            TraceContext::parse(
                "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
                None
            ),
            Err(TraceContextError::InvalidTraceParent)
        );
        assert_eq!(
            TraceContext::parse(
                "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None
            ),
            Err(TraceContextError::UnsupportedVersion)
        );
    }

    #[test]
    fn recent_trace_ring_aggregates_searches_and_bounds_spans() {
        let root = TraceContext::new_root(true);
        let message_id = format!("message-{}", Uuid::new_v4());
        let started_at = chrono::Utc::now();

        for index in 0..(SPANS_PER_TRACE_LIMIT + 2) {
            let context = root.child();
            record_span(
                &context,
                TraceSpan {
                    name: format!("span-{index}"),
                    span_id: context.span_id.clone(),
                    parent_span_id: Some(root.span_id.clone()),
                    started_at,
                    completed_at: started_at,
                    duration_us: 0,
                    status: if index == SPANS_PER_TRACE_LIMIT + 1 {
                        TraceSpanStatus::Error
                    } else {
                        TraceSpanStatus::Ok
                    },
                    attributes: TraceAttributes {
                        message_id: Some(message_id.clone()),
                        ..Default::default()
                    },
                },
            );
        }

        let trace = recent_trace(&root.trace_id).expect("trace should be retained");
        assert_eq!(trace.span_count, SPANS_PER_TRACE_LIMIT + 2);
        assert_eq!(trace.spans.len(), SPANS_PER_TRACE_LIMIT);
        assert_eq!(trace.dropped_spans, 2);
        assert_eq!(trace.error_count, 1);
        assert_eq!(trace.spans[0].name, "span-2");
        assert_eq!(
            search_recent_traces(&message_id)
                .into_iter()
                .map(|summary| summary.trace_id)
                .collect::<Vec<_>>(),
            vec![root.trace_id]
        );
    }
}
