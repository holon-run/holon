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
