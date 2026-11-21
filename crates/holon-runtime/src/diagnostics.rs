pub use holon_observability::diagnostics::*;

pub mod io_probe;
pub(crate) mod resource_io;

pub fn record_runtime_db_wal_checkpoint(
    report: &crate::runtime_db::retention::RuntimeDbWalCheckpointReport,
) {
    holon_observability::diagnostics::record_runtime_db_wal_checkpoint(
        RuntimeDbWalCheckpointDiagnostics {
            last_pass_at: Some(report.started_at.to_rfc3339()),
            wal_bytes_before: report.wal_bytes_before,
            wal_bytes_after: report.wal_bytes_after,
            busy: report.busy,
            log_frames: report.log_frames,
            checkpointed_frames: report.checkpointed_frames,
            elapsed_ms: report.elapsed_ms,
        },
    );
}
