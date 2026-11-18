use std::collections::HashSet;

use super::*;

#[test]
fn renderer_uses_bounded_label_free_series() {
    let rendered = render(&crate::diagnostics::performance_snapshot());
    let samples = rendered
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    let (reader_pool_samples, label_free_samples): (Vec<&str>, Vec<&str>) = samples
        .iter()
        .copied()
        .partition(|line| line.starts_with("holon_runtime_db_reader_pool_"));
    let names = label_free_samples
        .iter()
        .map(|line| line.split_once(' ').expect("sample value").0)
        .collect::<HashSet<_>>();

    assert!(rendered.ends_with("# EOF\n"));
    assert_eq!(
        names.len(),
        label_free_samples.len(),
        "label-free series names must be unique"
    );
    assert!(
        samples.len() < 900,
        "series count must remain bounded, got {}",
        samples.len()
    );
    assert!(label_free_samples.iter().all(|line| !line.contains('{')));
    assert_eq!(
        reader_pool_samples.len(),
        2 * (6 + 14 + 3 * 29),
        "reader-pool series keep a fixed role, metric, and bucket shape"
    );
    assert!(names.contains("holon_otlp_exporter_queue_depth"));
    assert!(names.contains("holon_otlp_exporter_exported_spans_total"));
    assert!(names.contains("holon_otlp_exporter_failed_batches_total"));
    assert!(names.contains("holon_memory_index_writer_queue_depth"));
    assert!(names.contains("holon_tool_execution_bytes_total"));
    assert!(names.contains("holon_memory_index_writer_foreground_timeouts_total"));
    assert!(names.contains("holon_memory_index_writer_maintenance_timeouts_total"));
    assert!(names
        .contains("holon_memory_index_writer_rebuild_scan_queue_wait_duration_milliseconds_p95"));
    assert!(names.contains("holon_memory_index_writer_rebuild_scan_queue_wait_timeouts_total"));
    assert!(names.iter().all(|name| {
        name.chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    }));
}

#[test]
fn metric_names_do_not_preserve_dynamic_label_syntax() {
    assert_eq!(
        metric_name("http.json./agents/{agent_id}/status"),
        "http_json_agents_agent_id_status"
    );
}

#[test]
fn renders_reader_pool_families_with_only_fixed_labels() {
    use std::time::Duration;

    use crate::diagnostics::reader_pools::ReaderPoolAccumulator;
    use crate::diagnostics::{ReaderPoolEvent, ReaderPoolOutcome, ReaderPoolRole};

    let accumulator = ReaderPoolAccumulator::new();
    accumulator.record(
        ReaderPoolRole::Ordinary,
        ReaderPoolEvent::Created { capacity: 16 },
    );
    accumulator.record(ReaderPoolRole::Ordinary, ReaderPoolEvent::ConnectionOpened);
    accumulator.record(ReaderPoolRole::Ordinary, ReaderPoolEvent::CheckoutStarted);
    accumulator.record(
        ReaderPoolRole::Ordinary,
        ReaderPoolEvent::CheckoutFinished {
            elapsed: Duration::from_millis(5),
            outcome: ReaderPoolOutcome::Success,
        },
    );
    let mut snapshot = crate::diagnostics::performance_snapshot();
    snapshot.reader_pools = accumulator.snapshot();

    let rendered = render(&snapshot);
    assert!(rendered.contains(
        "holon_runtime_db_reader_pool_instances{resource_scope=\"runtime_db\",role=\"ordinary\"} 1"
    ));
    assert!(rendered.contains(
        "holon_runtime_db_reader_pool_configured_capacity{resource_scope=\"runtime_db\",role=\"preparation\"} 0"
    ));
    assert!(rendered.contains(
        "holon_runtime_db_reader_pool_checkout_duration_seconds_bucket{resource_scope=\"runtime_db\",role=\"ordinary\",le=\"+Inf\"} 1"
    ));
    assert!(rendered.contains(
        "holon_runtime_db_reader_pool_checkout_duration_seconds_sum{resource_scope=\"runtime_db\",role=\"ordinary\"} 0.005"
    ));
    assert!(rendered.contains(
        "holon_runtime_db_reader_pool_checkout_duration_seconds_count{resource_scope=\"runtime_db\",role=\"ordinary\"} 1"
    ));

    for line in rendered
        .lines()
        .filter(|line| line.starts_with("holon_runtime_db_reader_pool_"))
    {
        let (_, rest) = line.split_once('{').expect("reader-pool labels");
        let (labels, _) = rest.split_once('}').expect("reader-pool label close");
        for pair in labels.split(',') {
            let (key, value) = pair.split_once('=').expect("label pair");
            assert!(
                matches!(key, "resource_scope" | "role" | "le"),
                "unexpected reader-pool label {key}"
            );
            if key == "resource_scope" {
                assert_eq!(value, "\"runtime_db\"");
            }
        }
    }
}
