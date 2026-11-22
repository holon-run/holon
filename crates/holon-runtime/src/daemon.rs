mod runtime;
mod service;
pub use holon_daemon::logging;

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests;

pub use holon_daemon::lifecycle::{
    daemon_launch_with_profile, daemon_prepare_update, daemon_restart, daemon_restart_with_timeout,
    daemon_start, daemon_start_with_timeout, daemon_status, daemon_stop, ensure_serve_preflight,
    prepare_serve_with_profile, DAEMON_SERVE_ARGS_ENV, DAEMON_SERVE_RESOLUTION_ENV,
    PRE_SERVER_PREPARED_ENV,
};
pub use holon_daemon::profile::{
    load_serve_profile, persist_serve_profile, serve_profile_path, validate_cloudflare_url,
    DaemonServeProfile, ServeProfileAccess, ServeProfileAuthentication,
};
pub(crate) use holon_daemon::quarantine::{
    persist_quarantine_boot, QuarantineBoot, QuarantineEntry, QuarantineReason, QuarantineStage,
};
pub use holon_daemon::quarantine::{
    read_quarantine_report, QuarantineReport, QuarantineReportUnavailable,
};
pub use holon_daemon::service::{
    RuntimeActivityState, RuntimeActivitySummary, RuntimeConfigSurface, RuntimeControlAuthMode,
    RuntimeServiceHandle, RuntimeServiceMetadata, RuntimeShutdownResponse, RuntimeStartupSurface,
    RuntimeStatusResponse, RuntimeWebSearchSummary, DAEMON_CONTROL_PROTOCOL_VERSION,
};
pub use holon_daemon::state::{
    cleanup_daemon_state, daemon_logs, daemon_paths, load_daemon_metadata,
    load_last_runtime_failure, DaemonDesiredServe, DaemonLifecycleAction, DaemonLifecycleOwner,
    DaemonLifecycleResult, DaemonLifecycleState, DaemonLogsView, DaemonPaths, DaemonStatusView,
};
pub use runtime::{
    config_fingerprint, graceful_runtime_shutdown, prepare_runtime_before_server,
    prepare_runtime_before_server_observed,
    prepare_runtime_before_server_observed_with_maintenance_lease,
    prepare_runtime_before_server_with_maintenance_lease,
};
pub use service::activity_from_snapshots;
pub use service::runtime_activity_summary;

pub type ResolvedDaemonServePlan = holon_daemon::ResolvedDaemonServePlan<crate::config::AppConfig>;
