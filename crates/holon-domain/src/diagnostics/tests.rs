use super::*;

#[test]
fn performance_snapshot_without_reader_pools_deserializes() {
    let legacy = r#"{
        "captured_at": "2026-01-01T00:00:00+00:00",
        "process_uptime_ms": 1,
        "http": [],
        "projections": [],
        "projection_gate": {
            "leaders": 0, "joined_waiters": 0, "cache_hits": 0,
            "cache_misses": 0, "rejected": 0, "failed": 0, "cancelled": 0,
            "stale_served": 0, "stale_expired": 0, "active_permits": 0,
            "max_active_permits": 0
        },
        "db": [],
        "scheduler": [],
        "turn": [],
        "provider": []
    }"#;
    let snapshot: PerformanceDiagnosticsSnapshot =
        serde_json::from_str(legacy).expect("legacy snapshot without reader_pools");
    assert!(snapshot.reader_pools.is_empty());
}

#[test]
fn reader_pool_roles_serialize_as_fixed_names() {
    assert_eq!(
        serde_json::to_string(&ReaderPoolRole::Ordinary).unwrap(),
        "\"ordinary\""
    );
    assert_eq!(
        serde_json::to_string(&ReaderPoolRole::Preparation).unwrap(),
        "\"preparation\""
    );
    assert_eq!(ReaderPoolRole::default(), ReaderPoolRole::Ordinary);
}

#[test]
fn trace_attributes_without_usage_deserialize() {
    // Historical trace rows written before `attempt_token_usage.v1` predate the
    // nested `usage` object; they must keep deserializing with `usage: None`.
    let legacy = r#"{
        "agent_id": "a1",
        "provider": "gemini",
        "input_tokens": 5000,
        "output_tokens": 500,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0
    }"#;
    let attributes: TraceAttributes =
        serde_json::from_str(legacy).expect("legacy TraceAttributes without usage");
    assert!(attributes.usage.is_none());
    assert_eq!(attributes.input_tokens, Some(5000));
}

#[test]
fn trace_token_usage_round_trips_with_semantics() {
    let usage = TraceTokenUsage {
        uncached_input_tokens: 4000,
        cache_read_tokens: 1000,
        cache_write_tokens: 0,
        output_tokens: 500,
        reasoning_tokens: Some(100),
        semantics: "attempt_token_usage.v1".to_string(),
    };
    let json = serde_json::to_string(&usage).unwrap();
    let back: TraceTokenUsage = serde_json::from_str(&json).unwrap();
    assert_eq!(back, usage);
}
