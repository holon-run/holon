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
