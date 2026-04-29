mod lifecycle;
mod service;
mod state;

#[cfg(test)]
mod tests;

pub use lifecycle::{
    daemon_restart, daemon_start, daemon_status, daemon_stop, ensure_serve_preflight,
    graceful_runtime_shutdown,
};
pub(crate) use service::runtime_activity_message;
pub use service::{
    runtime_activity_summary, RuntimeActivityState, RuntimeActivitySummary,
    RuntimeAgentOverrideSummary, RuntimeConfigSurface, RuntimeServiceHandle,
    RuntimeServiceMetadata, RuntimeShutdownResponse, RuntimeStartupSurface, RuntimeStatusResponse,
};
pub use state::{
    cleanup_daemon_state, config_fingerprint, daemon_logs, daemon_paths, load_daemon_metadata,
    load_last_runtime_failure, DaemonLifecycleAction, DaemonLifecycleResult, DaemonLifecycleState,
    DaemonLogsView, DaemonPaths, DaemonStatusView,
};
pub(crate) use state::{
    clear_persisted_daemon_lifecycle_failures, daemon_log_hint, persist_daemon_lifecycle_failure,
    read_daemon_log_excerpt, stale_files,
};
