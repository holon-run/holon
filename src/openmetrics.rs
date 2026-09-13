use std::fmt::Write as _;

use crate::diagnostics::{MetricSnapshot, PerformanceDiagnosticsSnapshot};

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
    {
        render_metric(&mut output, metric);
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
        let names = samples
            .iter()
            .map(|line| line.split_once(' ').expect("sample value").0)
            .collect::<HashSet<_>>();

        assert!(rendered.ends_with("# EOF\n"));
        assert_eq!(names.len(), samples.len(), "series names must be unique");
        assert!(samples.len() < 500, "series count must remain bounded");
        assert!(samples.iter().all(|line| !line.contains('{')));
        assert!(names.contains("holon_otlp_exporter_queue_depth"));
        assert!(names.contains("holon_otlp_exporter_exported_spans_total"));
        assert!(names.contains("holon_otlp_exporter_failed_batches_total"));
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
}
