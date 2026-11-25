pub use holon_domain::diagnostics::{
    RecentTrace, RecentTraceSummary, TraceAttributes, TraceSpan, TraceSpanStatus, TraceTokenUsage,
};

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

const RECENT_TRACE_LIMIT: usize = 128;
const SPANS_PER_TRACE_LIMIT: usize = 256;

static RECENT_TRACES: OnceLock<Mutex<RecentTraceStore>> = OnceLock::new();

pub use holon_domain::{TraceContext, TraceContextError};

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

#[cfg(test)]
mod tests;
