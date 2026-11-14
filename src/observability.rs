use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

const RECENT_TRACE_LIMIT: usize = 128;
const SPANS_PER_TRACE_LIMIT: usize = 256;

static RECENT_TRACES: OnceLock<Mutex<RecentTraceStore>> = OnceLock::new();

pub use holon_domain::{TraceContext, TraceContextError};

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
    let trace = {
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
        store.traces.push_front(trace.clone());
        store.traces.truncate(RECENT_TRACE_LIMIT);
        trace
    };
    if let Some(span) = trace.spans.last() {
        crate::otlp_exporter::try_record_span(context, span);
    }
    crate::diagnostics_store::try_record_trace(trace);
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

#[cfg(holon_lib_test_shard = "d")]
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
        let message_id = format!("message-{}", uuid::Uuid::new_v4());
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
