use std::fmt::Write as _;

use crate::diagnostics::{
    MetricSnapshot, PerformanceDiagnosticsSnapshot, ReaderPoolDiagnosticsSnapshot,
    ReaderPoolDurationSnapshot, ReaderPoolRole,
};

pub const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

pub fn render(snapshot: &PerformanceDiagnosticsSnapshot) -> String {
    let mut output = String::new();
    gauge(
        &mut output,
        "holon_process_uptime_seconds",
        snapshot.process_uptime_ms as f64 / 1_000.0,
    );

    for metric in snapshot
        .http
        .iter()
        .chain(&snapshot.projections)
        .chain(&snapshot.db)
        .chain(&snapshot.scheduler)
        .chain(&snapshot.turn)
        .chain(&snapshot.provider)
        .chain(&snapshot.tools)
        .chain(&snapshot.conversation.queries)
    {
        render_metric(&mut output, metric);
    }

    render_reader_pools(&mut output, &snapshot.reader_pools);

    if snapshot.runtime_db_wal.last_pass_at.is_some() {
        gauge(
            &mut output,
            "holon_runtime_db_wal_bytes",
            snapshot.runtime_db_wal.wal_bytes_after,
        );
    }

    let writer = &snapshot.memory_index_writer;
    gauge(
        &mut output,
        "holon_memory_index_writer_queue_depth",
        writer.queue_depth,
    );
    gauge(
        &mut output,
        "holon_memory_index_writer_max_queue_depth",
        writer.max_queue_depth,
    );
    gauge(
        &mut output,
        "holon_memory_index_writer_foreground_queue_depth",
        writer.foreground_queue_depth,
    );
    gauge(
        &mut output,
        "holon_memory_index_writer_max_foreground_queue_depth",
        writer.max_foreground_queue_depth,
    );
    gauge(
        &mut output,
        "holon_memory_index_writer_maintenance_queue_depth",
        writer.maintenance_queue_depth,
    );
    gauge(
        &mut output,
        "holon_memory_index_writer_max_maintenance_queue_depth",
        writer.max_maintenance_queue_depth,
    );
    counter(
        &mut output,
        "holon_memory_index_writer_foreground_timeouts_total",
        writer.foreground_timeouts,
    );
    counter(
        &mut output,
        "holon_memory_index_writer_maintenance_timeouts_total",
        writer.maintenance_timeouts,
    );
    for operation in &writer.operations {
        render_metric(&mut output, &operation.queue_wait);
        let prefix = format!("holon_{}", metric_name(&operation.queue_wait.name));
        counter(
            &mut output,
            &format!("{prefix}_timeouts_total"),
            operation.timeouts,
        );
    }

    let gate = &snapshot.projection_gate;
    counter(
        &mut output,
        "holon_projection_gate_leaders_total",
        gate.leaders,
    );
    counter(
        &mut output,
        "holon_projection_gate_joined_waiters_total",
        gate.joined_waiters,
    );
    counter(
        &mut output,
        "holon_projection_gate_cache_hits_total",
        gate.cache_hits,
    );
    counter(
        &mut output,
        "holon_projection_gate_cache_misses_total",
        gate.cache_misses,
    );
    counter(
        &mut output,
        "holon_projection_gate_rejected_total",
        gate.rejected,
    );
    counter(
        &mut output,
        "holon_projection_gate_failed_total",
        gate.failed,
    );
    counter(
        &mut output,
        "holon_projection_gate_cancelled_total",
        gate.cancelled,
    );
    counter(
        &mut output,
        "holon_projection_gate_stale_served_total",
        gate.stale_served,
    );
    counter(
        &mut output,
        "holon_projection_gate_stale_expired_total",
        gate.stale_expired,
    );
    gauge(
        &mut output,
        "holon_projection_gate_active_permits",
        gate.active_permits,
    );
    gauge(
        &mut output,
        "holon_projection_gate_max_active_permits",
        gate.max_active_permits,
    );

    let conversation = &snapshot.conversation;
    counter(
        &mut output,
        "holon_conversation_capability_unavailable_total",
        conversation.capability_unavailable,
    );
    counter(
        &mut output,
        "holon_conversation_cursor_failures_total",
        conversation.cursor_failures,
    );
    counter(
        &mut output,
        "holon_conversation_limit_failures_total",
        conversation.limit_failures,
    );
    counter(
        &mut output,
        "holon_conversation_payload_failures_total",
        conversation.payload_failures,
    );
    counter(
        &mut output,
        "holon_conversation_timeouts_total",
        conversation.timeouts,
    );
    counter(
        &mut output,
        "holon_conversation_slow_consumers_total",
        conversation.slow_consumers,
    );
    counter(
        &mut output,
        "holon_conversation_legacy_unattributed_briefs_total",
        conversation.legacy_unattributed_briefs,
    );
    counter(
        &mut output,
        "holon_conversation_shadow_matches_total",
        conversation.shadow_matches,
    );
    counter(
        &mut output,
        "holon_conversation_shadow_mismatches_total",
        conversation.shadow_mismatches,
    );
    let resets = &conversation.resets;
    counter(
        &mut output,
        "holon_conversation_resets_retention_expired_total",
        resets.retention_expired,
    );
    counter(
        &mut output,
        "holon_conversation_resets_cursor_ahead_total",
        resets.cursor_ahead,
    );
    counter(
        &mut output,
        "holon_conversation_resets_replay_limit_exceeded_total",
        resets.replay_limit_exceeded,
    );
    counter(
        &mut output,
        "holon_conversation_resets_agent_not_found_total",
        resets.agent_not_found,
    );
    counter(
        &mut output,
        "holon_conversation_resets_query_version_mismatch_total",
        resets.query_version_mismatch,
    );
    counter(
        &mut output,
        "holon_conversation_resets_schema_version_mismatch_total",
        resets.schema_version_mismatch,
    );
    counter(
        &mut output,
        "holon_conversation_resets_event_log_epoch_mismatch_total",
        resets.event_log_epoch_mismatch,
    );
    counter(
        &mut output,
        "holon_conversation_resets_visibility_scope_mismatch_total",
        resets.visibility_scope_mismatch,
    );
    counter(
        &mut output,
        "holon_conversation_resets_stream_recovery_failed_total",
        resets.stream_recovery_failed,
    );
    counter(
        &mut output,
        "holon_conversation_resets_slow_consumer_total",
        resets.slow_consumer,
    );

    let writer = snapshot.diagnostics_writer;
    gauge(
        &mut output,
        "holon_diagnostics_writer_queue_depth",
        writer.queue_depth,
    );
    counter(
        &mut output,
        "holon_diagnostics_writer_queued_traces_total",
        writer.queued_traces,
    );
    counter(
        &mut output,
        "holon_diagnostics_writer_dropped_traces_total",
        writer.dropped_traces,
    );
    counter(
        &mut output,
        "holon_diagnostics_writer_filtered_traces_total",
        writer.filtered_traces,
    );
    counter(
        &mut output,
        "holon_diagnostics_writer_persisted_traces_total",
        writer.persisted_traces,
    );
    counter(
        &mut output,
        "holon_diagnostics_writer_retention_deleted_traces_total",
        writer.retention_deleted_traces,
    );
    counter(
        &mut output,
        "holon_diagnostics_writer_failures_total",
        writer.writer_failures,
    );

    let exporter = crate::otlp_exporter::exporter_stats();
    gauge(
        &mut output,
        "holon_otlp_exporter_queue_depth",
        exporter.queue_depth,
    );
    counter(
        &mut output,
        "holon_otlp_exporter_queued_spans_total",
        exporter.queued_spans,
    );
    counter(
        &mut output,
        "holon_otlp_exporter_dropped_spans_total",
        exporter.dropped_spans,
    );
    counter(
        &mut output,
        "holon_otlp_exporter_exported_spans_total",
        exporter.exported_spans,
    );
    counter(
        &mut output,
        "holon_otlp_exporter_failed_batches_total",
        exporter.failed_batches,
    );

    output.push_str("# EOF\n");
    output
}

fn render_reader_pools(output: &mut String, pools: &[ReaderPoolDiagnosticsSnapshot]) {
    if pools.is_empty() {
        return;
    }

    let gauges: [(&str, fn(&ReaderPoolDiagnosticsSnapshot) -> u64); 6] = [
        ("holon_runtime_db_reader_pool_instances", |pool| {
            pool.instances
        }),
        ("holon_runtime_db_reader_pool_configured_capacity", |pool| {
            pool.configured_capacity
        }),
        ("holon_runtime_db_reader_pool_connections", |pool| {
            pool.connections
        }),
        ("holon_runtime_db_reader_pool_in_use", |pool| pool.in_use),
        ("holon_runtime_db_reader_pool_idle", |pool| pool.idle),
        ("holon_runtime_db_reader_pool_waiting", |pool| pool.waiting),
    ];
    for (name, value) in gauges {
        reader_pool_gauge_family(output, name, pools, value);
    }

    let counters: [(&str, fn(&ReaderPoolDiagnosticsSnapshot) -> u64); 14] = [
        (
            "holon_runtime_db_reader_pool_instances_created_total",
            |pool| pool.instances_created,
        ),
        (
            "holon_runtime_db_reader_pool_instances_dropped_total",
            |pool| pool.instances_dropped,
        ),
        (
            "holon_runtime_db_reader_pool_connections_opened_total",
            |pool| pool.connections_opened,
        ),
        (
            "holon_runtime_db_reader_pool_connections_closed_total",
            |pool| pool.connections_closed,
        ),
        (
            "holon_runtime_db_reader_pool_checkout_started_total",
            |pool| pool.checkouts_started,
        ),
        (
            "holon_runtime_db_reader_pool_checkout_succeeded_total",
            |pool| pool.checkouts_succeeded,
        ),
        (
            "holon_runtime_db_reader_pool_checkout_timed_out_total",
            |pool| pool.checkouts_timed_out,
        ),
        (
            "holon_runtime_db_reader_pool_checkout_cancelled_total",
            |pool| pool.checkouts_cancelled,
        ),
        (
            "holon_runtime_db_reader_pool_checkout_failed_total",
            |pool| pool.checkouts_failed,
        ),
        ("holon_runtime_db_reader_pool_lease_started_total", |pool| {
            pool.leases_started
        }),
        (
            "holon_runtime_db_reader_pool_lease_finished_total",
            |pool| pool.leases_finished,
        ),
        (
            "holon_runtime_db_reader_pool_connect_started_total",
            |pool| pool.connects_started,
        ),
        (
            "holon_runtime_db_reader_pool_connect_succeeded_total",
            |pool| pool.connects_succeeded,
        ),
        (
            "holon_runtime_db_reader_pool_connect_failed_total",
            |pool| pool.connects_failed,
        ),
    ];
    for (name, value) in counters {
        reader_pool_counter_family(output, name, pools, value);
    }

    for kind in [
        ReaderPoolDuration::Checkout,
        ReaderPoolDuration::Lease,
        ReaderPoolDuration::Connect,
    ] {
        reader_pool_histogram_family(output, kind.metric_base(), pools, kind);
    }
}

fn reader_pool_gauge_family(
    output: &mut String,
    name: &str,
    pools: &[ReaderPoolDiagnosticsSnapshot],
    value: fn(&ReaderPoolDiagnosticsSnapshot) -> u64,
) {
    writeln!(output, "# TYPE {name} gauge").expect("writing to a string cannot fail");
    for pool in pools {
        writeln!(
            output,
            "{name}{{{}}} {}",
            reader_pool_labels(pool.role),
            value(pool)
        )
        .expect("writing to a string cannot fail");
    }
}

fn reader_pool_counter_family(
    output: &mut String,
    name: &str,
    pools: &[ReaderPoolDiagnosticsSnapshot],
    value: fn(&ReaderPoolDiagnosticsSnapshot) -> u64,
) {
    writeln!(output, "# TYPE {name} counter").expect("writing to a string cannot fail");
    for pool in pools {
        writeln!(
            output,
            "{name}{{{}}} {}",
            reader_pool_labels(pool.role),
            value(pool)
        )
        .expect("writing to a string cannot fail");
    }
}

#[derive(Clone, Copy)]
enum ReaderPoolDuration {
    Checkout,
    Lease,
    Connect,
}

impl ReaderPoolDuration {
    fn metric_base(self) -> &'static str {
        match self {
            Self::Checkout => "holon_runtime_db_reader_pool_checkout_duration_seconds",
            Self::Lease => "holon_runtime_db_reader_pool_lease_duration_seconds",
            Self::Connect => "holon_runtime_db_reader_pool_connect_duration_seconds",
        }
    }

    fn select(self, pool: &ReaderPoolDiagnosticsSnapshot) -> &ReaderPoolDurationSnapshot {
        match self {
            Self::Checkout => &pool.checkout_duration,
            Self::Lease => &pool.lease_duration,
            Self::Connect => &pool.connect_duration,
        }
    }
}

fn reader_pool_histogram_family(
    output: &mut String,
    name: &str,
    pools: &[ReaderPoolDiagnosticsSnapshot],
    kind: ReaderPoolDuration,
) {
    writeln!(output, "# TYPE {name} histogram").expect("writing to a string cannot fail");
    for pool in pools {
        let labels = reader_pool_labels(pool.role);
        let duration = kind.select(pool);
        for bucket in &duration.buckets {
            writeln!(
                output,
                "{name}_bucket{{{labels},le=\"{}\"}} {}",
                reader_pool_le(bucket.upper_bound_ms),
                bucket.count
            )
            .expect("writing to a string cannot fail");
        }
        writeln!(
            output,
            "{name}_sum{{{labels}}} {}",
            duration.total_ns as f64 / 1_000_000_000.0
        )
        .expect("writing to a string cannot fail");
        writeln!(output, "{name}_count{{{labels}}} {}", duration.count)
            .expect("writing to a string cannot fail");
    }
}

/// Fixed scope and role labels are the only reader-pool dimensions.
fn reader_pool_labels(role: ReaderPoolRole) -> String {
    let role = match role {
        ReaderPoolRole::Ordinary => "ordinary",
        ReaderPoolRole::Preparation => "preparation",
    };
    format!("resource_scope=\"runtime_db\",role=\"{role}\"")
}

fn reader_pool_le(upper_bound_ms: Option<u64>) -> String {
    match upper_bound_ms {
        Some(upper_bound_ms) => format!("{}", upper_bound_ms as f64 / 1_000.0),
        None => "+Inf".to_string(),
    }
}

fn render_metric(output: &mut String, metric: &MetricSnapshot) {
    let prefix = format!("holon_{}", metric_name(&metric.name));
    counter(output, &format!("{prefix}_count_total"), metric.count);
    counter(
        output,
        &format!("{prefix}_duration_milliseconds_total"),
        metric.total_ms,
    );
    gauge(
        output,
        &format!("{prefix}_duration_milliseconds_max"),
        metric.max_ms,
    );
    gauge(
        output,
        &format!("{prefix}_duration_milliseconds_avg"),
        metric.avg_ms,
    );
    gauge(
        output,
        &format!("{prefix}_duration_milliseconds_p50"),
        metric.p50_ms,
    );
    gauge(
        output,
        &format!("{prefix}_duration_milliseconds_p95"),
        metric.p95_ms,
    );
    gauge(
        output,
        &format!("{prefix}_duration_milliseconds_p99"),
        metric.p99_ms,
    );
    if let Some(total_bytes) = metric.total_bytes {
        counter(output, &format!("{prefix}_bytes_total"), total_bytes);
    }
    if let Some(avg_bytes) = metric.avg_bytes {
        gauge(output, &format!("{prefix}_bytes_avg"), avg_bytes);
    }
}

fn metric_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    let mut previous_was_separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            normalized.push('_');
            previous_was_separator = true;
        }
    }
    normalized.trim_matches('_').to_string()
}

fn counter(output: &mut String, name: &str, value: u64) {
    writeln!(output, "# TYPE {name} counter").expect("writing to a string cannot fail");
    writeln!(output, "{name} {value}").expect("writing to a string cannot fail");
}

fn gauge(output: &mut String, name: &str, value: impl std::fmt::Display) {
    writeln!(output, "# TYPE {name} gauge").expect("writing to a string cannot fail");
    writeln!(output, "{name} {value}").expect("writing to a string cannot fail");
}

#[cfg(test)]
mod tests {
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
        assert!(names.contains(
            "holon_memory_index_writer_rebuild_scan_queue_wait_duration_milliseconds_p95"
        ));
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
}
