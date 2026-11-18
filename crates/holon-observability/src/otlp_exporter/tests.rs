use std::time::{Duration, Instant};

use chrono::Utc;

use super::*;
use crate::observability::TraceAttributes;

#[test]
fn otlp_attribute_value_preserves_numeric_types() {
    assert_eq!(
        otlp_attribute_value(json!(-7)),
        Some(json!({"intValue": "-7"}))
    );
    assert_eq!(
        otlp_attribute_value(json!(u64::MAX)),
        Some(json!({"intValue": u64::MAX.to_string()}))
    );
    assert_eq!(
        otlp_attribute_value(json!(3.25)),
        Some(json!({"doubleValue": 3.25}))
    );
}

#[test]
fn exporter_failure_is_counted_and_shutdown_drains_worker() {
    let config = OtlpExporterConfig {
        endpoint: "http://127.0.0.1:9/v1/traces".to_string(),
        queue_capacity: 4,
        batch_size: 1,
        batch_interval: Duration::from_millis(10),
        timeout: Duration::from_millis(100),
        ..OtlpExporterConfig::default()
    };
    let exporter = OtlpExporter::start(config).unwrap();
    let handle = exporter.handle.as_ref().unwrap().clone();
    let now = Utc::now();
    handle.try_send(ExportSpan {
        context: TraceContext {
            trace_id: "0123456789abcdef0123456789abcdef".to_string(),
            span_id: "0123456789abcdef".to_string(),
            trace_flags: 1,
            trace_state: None,
        },
        span: TraceSpan {
            name: "test".to_string(),
            span_id: "fedcba9876543210".to_string(),
            parent_span_id: None,
            started_at: now,
            completed_at: now,
            duration_us: 1,
            status: TraceSpanStatus::Ok,
            attributes: TraceAttributes::default(),
        },
    });

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let stats = exporter.stats();
        if stats.failed_batches == 1 {
            assert_eq!(stats.queued_spans, 1);
            assert_eq!(stats.exported_spans, 0);
            assert_eq!(stats.dropped_spans, 1);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "OTLP failure was not observed: {stats:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }

    exporter.shutdown().unwrap();
}
