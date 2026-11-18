use super::*;
use crate::observability::{completed_span_at, TraceAttributes, TraceContext, TraceSpanStatus};
use chrono::{TimeZone, Utc};

fn trace(trace_id: &str, duration_ms: i64, status: TraceSpanStatus) -> RecentTrace {
    let started_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
    let completed_at = started_at + chrono::Duration::milliseconds(duration_ms);
    let context = TraceContext {
        trace_id: trace_id.to_string(),
        span_id: "0011223344556677".to_string(),
        trace_flags: 1,
        trace_state: None,
    };
    let span = completed_span_at(
        "holon.turn",
        &context,
        None,
        started_at,
        completed_at,
        status,
        TraceAttributes {
            turn_id: Some("turn-1".to_string()),
            message_id: Some("message-1".to_string()),
            ..Default::default()
        },
    );
    RecentTrace {
        trace_id: trace_id.to_string(),
        started_at,
        completed_at,
        duration_us: duration_ms as u64 * 1_000,
        span_count: 1,
        dropped_spans: 0,
        error_count: usize::from(status == TraceSpanStatus::Error),
        spans: vec![span],
    }
}

#[test]
fn writer_persists_error_and_slow_traces_but_not_fast_successes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagnostics.sqlite");
    let writer = DiagnosticsWriter::start(
        &path,
        DiagnosticsWriterConfig {
            slow_trace_threshold: Duration::from_millis(500),
            ..Default::default()
        },
    )
    .unwrap();
    let handle = writer.handle();
    assert!(handle.try_record_trace(trace("fast", 10, TraceSpanStatus::Ok)));
    assert!(handle.try_record_trace(trace("slow", 600, TraceSpanStatus::Ok)));
    assert!(handle.try_record_trace(trace("error", 10, TraceSpanStatus::Error)));
    writer.shutdown().unwrap();

    assert!(persistent_trace(&path, "fast").unwrap().is_none());
    assert!(persistent_trace(&path, "slow").unwrap().is_some());
    assert!(persistent_trace(&path, "error").unwrap().is_some());
    assert_eq!(
        search_persistent_traces(&path, "turn-1", 10).unwrap().len(),
        2
    );
    assert_eq!(
        search_persistent_traces(&path, "message-1", 10)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn shutdown_drains_queued_updates_and_reopen_reads_latest_trace() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagnostics.sqlite");
    let writer = DiagnosticsWriter::start(
        &path,
        DiagnosticsWriterConfig {
            sample_rate_permyriad: 10_000,
            ..Default::default()
        },
    )
    .unwrap();
    let handle = writer.handle();
    assert!(handle.try_record_trace(trace("sampled", 10, TraceSpanStatus::Ok)));
    assert!(handle.try_record_trace(trace("sampled", 20, TraceSpanStatus::Ok)));
    writer.shutdown().unwrap();

    let persisted = persistent_trace(&path, "sampled").unwrap().unwrap();
    assert_eq!(persisted.duration_us, 20_000);
}

#[test]
fn full_queue_drops_without_blocking() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = DiagnosticsHandle {
        sender,
        counters: Arc::new(DiagnosticsWriterCounters::default()),
    };
    assert!(handle.try_record_trace(trace("first", 10, TraceSpanStatus::Ok)));
    assert!(!handle.try_record_trace(trace("second", 10, TraceSpanStatus::Ok)));
    assert_eq!(handle.stats().queued_traces, 1);
    assert_eq!(handle.stats().dropped_traces, 1);
    assert_eq!(handle.stats().queue_depth, 1);
    drop(receiver);
}

#[test]
fn persistent_search_treats_like_wildcards_as_literals() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagnostics.sqlite");
    let mut connection = open_connection(&path).unwrap();
    let transaction = connection.transaction().unwrap();
    persist_trace(
        &transaction,
        &trace("trace-percent%", 10, TraceSpanStatus::Error),
    )
    .unwrap();
    persist_trace(
        &transaction,
        &trace("trace-plain", 10, TraceSpanStatus::Error),
    )
    .unwrap();
    transaction.commit().unwrap();

    let matches = search_persistent_traces(&path, "%", 10).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].trace_id, "trace-percent%");
}

#[test]
fn shutting_down_old_writer_preserves_new_installed_handle() {
    let first_directory = tempfile::tempdir().unwrap();
    let second_directory = tempfile::tempdir().unwrap();
    let first = DiagnosticsWriter::start(
        first_directory.path().join("diagnostics.sqlite"),
        DiagnosticsWriterConfig {
            sample_rate_permyriad: 10_000,
            ..Default::default()
        },
    )
    .unwrap();
    let second_path = second_directory.path().join("diagnostics.sqlite");
    let second = DiagnosticsWriter::start(
        &second_path,
        DiagnosticsWriterConfig {
            sample_rate_permyriad: 10_000,
            ..Default::default()
        },
    )
    .unwrap();
    first.install();
    second.install();

    first.shutdown().unwrap();
    try_record_trace(trace("new-writer", 10, TraceSpanStatus::Ok));
    second.shutdown().unwrap();

    assert!(persistent_trace(&second_path, "new-writer")
        .unwrap()
        .is_some());
}

#[test]
fn dropping_writer_drains_queued_updates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagnostics.sqlite");
    let writer = DiagnosticsWriter::start(
        &path,
        DiagnosticsWriterConfig {
            sample_rate_permyriad: 10_000,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(writer
        .handle()
        .try_record_trace(trace("drop-drain", 10, TraceSpanStatus::Ok)));
    drop(writer);

    assert!(persistent_trace(&path, "drop-drain").unwrap().is_some());
}

#[test]
fn retention_respects_delete_batch_and_recent_floor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagnostics.sqlite");
    let mut connection = open_connection(&path).unwrap();
    let transaction = connection.transaction().unwrap();
    for trace_id in ["first", "second", "third", "fourth"] {
        persist_trace(&transaction, &trace(trace_id, 10, TraceSpanStatus::Error)).unwrap();
    }
    let config = DiagnosticsWriterConfig {
        retention_age: Duration::from_secs(100 * 365 * 24 * 60 * 60),
        retention_max_traces: 2,
        retention_min_traces: 1,
        retention_delete_batch: 1,
        ..Default::default()
    };

    assert_eq!(enforce_retention(&transaction, config).unwrap(), 1);
    assert_eq!(trace_count(&transaction).unwrap(), 3);
    assert_eq!(enforce_retention(&transaction, config).unwrap(), 1);
    assert_eq!(trace_count(&transaction).unwrap(), 2);
    assert_eq!(enforce_retention(&transaction, config).unwrap(), 0);
    transaction.commit().unwrap();
}

#[test]
fn writer_stats_report_persisted_filtered_and_retention_counts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagnostics.sqlite");
    let writer = DiagnosticsWriter::start(
        &path,
        DiagnosticsWriterConfig {
            slow_trace_threshold: Duration::from_millis(500),
            retention_age: Duration::from_secs(100 * 365 * 24 * 60 * 60),
            retention_max_traces: 1,
            retention_min_traces: 1,
            retention_delete_batch: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let handle = writer.handle();
    assert!(handle.try_record_trace(trace("fast", 10, TraceSpanStatus::Ok)));
    assert!(handle.try_record_trace(trace("error", 10, TraceSpanStatus::Error)));
    assert!(handle.try_record_trace(trace("slow", 600, TraceSpanStatus::Ok)));
    writer.shutdown().unwrap();

    let stats = handle.stats();
    assert_eq!(stats.queued_traces, 3);
    assert_eq!(stats.filtered_traces, 1);
    assert_eq!(stats.persisted_traces, 2);
    assert_eq!(stats.retention_deleted_traces, 1);
    assert_eq!(stats.dropped_traces, 0);
    assert_eq!(stats.writer_failures, 0);
}
