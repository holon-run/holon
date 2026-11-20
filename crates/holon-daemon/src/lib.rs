//! Standalone process ownership, serve intent, and local daemon diagnostics.

use std::path::{Path, PathBuf};

use anyhow::Result;
use holon_client::ConnectionConfig;
use holon_domain::RuntimeFailureSummary;

pub mod cloudflare_tunnel;
pub mod lifecycle;
pub mod logging;
pub mod profile;
pub mod quarantine;
pub mod service;
pub mod startup;
pub mod state;

pub use lifecycle::*;
pub use profile::*;
pub use quarantine::*;
pub use service::*;
pub use state::*;

/// Resolved launch settings and runtime operations supplied by the application.
pub trait DaemonConfig: Sync {
    fn home_dir(&self) -> &Path;
    fn run_dir(&self) -> PathBuf;
    fn socket_path(&self) -> &Path;
    fn http_addr(&self) -> &str;
    fn connection_config(&self) -> ConnectionConfig;
    fn control_auth_mode_env(&self) -> &'static str;
    fn required_tcp_control_token(&self) -> Option<&str>;
    fn product_version(&self) -> &'static str;
    fn config_fingerprint(&self) -> Result<String>;
    fn effective_config_mismatch_summary(&self, status: &RuntimeStatusResponse) -> String;

    fn prepare_runtime_before_server(&self, startup: &startup::StartupObservation) -> Result<()>;

    /// Used for log and shutdown diagnostics. Ordinary status uses the daemon's
    /// persisted diagnostics and never calls this operation.
    fn latest_public_runtime_failure(&self) -> Result<Option<RuntimeFailureSummary>>;
}

#[cfg(test)]
mod tests;
