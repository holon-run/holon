mod lifecycle;
mod service;
mod state;

#[cfg(test)]
mod tests;

pub use lifecycle::{
    daemon_prepare_update, daemon_restart, daemon_restart_with_timeout, daemon_start,
    daemon_start_with_timeout, daemon_status, daemon_stop, ensure_serve_preflight,
    graceful_runtime_shutdown, prepare_runtime_before_server, DAEMON_SERVE_ARGS_ENV,
    PRE_SERVER_PREPARED_ENV,
};
pub(crate) use service::runtime_activity_message;
pub use service::{
    runtime_activity_summary, RuntimeActivityState, RuntimeActivitySummary, RuntimeConfigSurface,
    RuntimeControlAuthMode, RuntimeServiceHandle, RuntimeServiceMetadata, RuntimeShutdownResponse,
    RuntimeStartupSurface, RuntimeStatusResponse, RuntimeWebSearchSummary,
    DAEMON_CONTROL_PROTOCOL_VERSION,
};
pub use state::{
    cleanup_daemon_state, config_fingerprint, daemon_logs, daemon_paths, load_daemon_metadata,
    load_last_runtime_failure, DaemonLifecycleAction, DaemonLifecycleOwner, DaemonLifecycleResult,
    DaemonLifecycleState, DaemonLogsView, DaemonPaths, DaemonStatusView,
};
pub(crate) use state::{
    clear_persisted_daemon_lifecycle_failures, daemon_log_hint, load_daemon_desired_running,
    persist_daemon_desired_running, persist_daemon_lifecycle_failure, read_daemon_log_excerpt,
    stale_files,
};
