pub use holon_client::wire::{
    RuntimeActivityState, RuntimeActivitySummary, RuntimeConfigSurface, RuntimeControlAuthMode,
    RuntimeDecisionLocalOnnxSurface, RuntimeDecisionSurface, RuntimeDecisionToolsSurface,
    RuntimeProviderDiscoverySummary, RuntimeProviderSummary, RuntimeShutdownResponse,
    RuntimeStartupSurface, RuntimeStatusResponse, RuntimeWebSearchProviderKindSummary,
    RuntimeWebSearchProviderSummary, RuntimeWebSearchSummary,
};

use std::{
    env, fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use holon_domain::RuntimeFailureSummary;

use crate::DaemonConfig;

use super::{daemon_paths, DaemonLifecycleOwner};

pub const DAEMON_CONTROL_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeServiceMetadata {
    pub pid: u32,
    pub home_dir: PathBuf,
    pub socket_path: PathBuf,
    pub http_addr: String,
    pub started_at: DateTime<Utc>,
    pub config_fingerprint: String,
    #[serde(default)]
    pub product_version: String,
    #[serde(default)]
    pub control_protocol_version: u32,
    #[serde(default)]
    pub lifecycle_owner: DaemonLifecycleOwner,
    #[serde(default)]
    pub executable_path: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serve_args: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control_token_env_configured: bool,
}

#[derive(Debug, Clone)]
pub struct RuntimeServiceHandle {
    inner: Arc<RuntimeServiceInner>,
}

#[derive(Debug)]
struct RuntimeServiceInner {
    metadata: RuntimeServiceMetadata,
    shutdown_tx: watch::Sender<bool>,
    healthy: AtomicBool,
    tunnel: RwLock<Option<crate::cloudflare_tunnel::TunnelStatusHandle>>,
}

impl RuntimeServiceHandle {
    pub fn new(config: &dyn DaemonConfig) -> Result<Self> {
        Self::new_with_health(config, true)
    }

    pub fn new_starting(config: &dyn DaemonConfig) -> Result<Self> {
        Self::new_with_health(config, false)
    }

    fn new_with_health(config: &dyn DaemonConfig, healthy: bool) -> Result<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(RuntimeServiceInner {
                metadata: RuntimeServiceMetadata {
                    pid: std::process::id(),
                    home_dir: config.home_dir().to_owned(),
                    socket_path: config.socket_path().to_owned(),
                    http_addr: config.http_addr().to_owned(),
                    started_at: Utc::now(),
                    config_fingerprint: config.config_fingerprint()?,
                    product_version: config.product_version().into(),
                    control_protocol_version: DAEMON_CONTROL_PROTOCOL_VERSION,
                    lifecycle_owner: DaemonLifecycleOwner::Standalone,
                    executable_path: env::current_exe()
                        .map_err(|err| anyhow!("failed to resolve current executable: {err}"))?,
                    serve_args: env::var(super::DAEMON_SERVE_ARGS_ENV)
                        .ok()
                        .and_then(|value| serde_json::from_str(&value).ok())
                        .unwrap_or_default(),
                    control_token_env_configured: env::var_os("HOLON_CONTROL_TOKEN").is_some(),
                },
                shutdown_tx,
                healthy: AtomicBool::new(healthy),
                tunnel: RwLock::new(None),
            }),
        })
    }

    pub fn mark_healthy(&self) {
        self.inner.healthy.store(true, Ordering::Release);
    }

    pub fn set_tunnel_status(&self, status: crate::cloudflare_tunnel::TunnelStatusHandle) {
        *self
            .inner
            .tunnel
            .write()
            .expect("tunnel status lock poisoned") = Some(status);
    }

    pub fn set_tunnel_failed(&self, public_url: &str) {
        self.set_tunnel_status(crate::cloudflare_tunnel::TunnelStatusHandle::failed(
            public_url,
        ));
    }

    fn tunnel_status(&self) -> Option<crate::cloudflare_tunnel::TunnelStatus> {
        self.inner
            .tunnel
            .read()
            .expect("tunnel status lock poisoned")
            .as_ref()
            .map(|status| status.snapshot())
    }

    pub fn status_response(
        &self,
        activity: RuntimeActivitySummary,
        last_failure: Option<RuntimeFailureSummary>,
        startup_surface: RuntimeStartupSurface,
        runtime_surface: RuntimeConfigSurface,
    ) -> RuntimeStatusResponse {
        RuntimeStatusResponse {
            ok: true,
            healthy: self.inner.healthy.load(Ordering::Acquire),
            tunnel: self.tunnel_status(),
            pid: self.inner.metadata.pid,
            home_dir: self.inner.metadata.home_dir.clone(),
            socket_path: self.inner.metadata.socket_path.clone(),
            http_addr: self.inner.metadata.http_addr.clone(),
            started_at: self.inner.metadata.started_at,
            config_fingerprint: self.inner.metadata.config_fingerprint.clone(),
            product_version: self.inner.metadata.product_version.clone(),
            control_protocol_version: self.inner.metadata.control_protocol_version,
            lifecycle_owner: self.inner.metadata.lifecycle_owner,
            executable_path: self.inner.metadata.executable_path.clone(),
            startup_surface: Some(startup_surface),
            runtime_surface: Some(runtime_surface),
            activity: Some(activity),
            last_failure,
            runtime_db_protection: None,
        }
    }

    pub fn readiness_response(
        &self,
        startup_surface: RuntimeStartupSurface,
        runtime_surface: RuntimeConfigSurface,
    ) -> RuntimeStatusResponse {
        RuntimeStatusResponse {
            ok: true,
            healthy: self.inner.healthy.load(Ordering::Acquire),
            tunnel: self.tunnel_status(),
            pid: self.inner.metadata.pid,
            home_dir: self.inner.metadata.home_dir.clone(),
            socket_path: self.inner.metadata.socket_path.clone(),
            http_addr: self.inner.metadata.http_addr.clone(),
            started_at: self.inner.metadata.started_at,
            config_fingerprint: self.inner.metadata.config_fingerprint.clone(),
            product_version: self.inner.metadata.product_version.clone(),
            control_protocol_version: self.inner.metadata.control_protocol_version,
            lifecycle_owner: self.inner.metadata.lifecycle_owner,
            executable_path: self.inner.metadata.executable_path.clone(),
            startup_surface: Some(startup_surface),
            runtime_surface: Some(runtime_surface),
            activity: None,
            last_failure: None,
            runtime_db_protection: None,
        }
    }

    pub fn shutdown_response(&self) -> RuntimeShutdownResponse {
        RuntimeShutdownResponse {
            ok: true,
            pid: self.inner.metadata.pid,
            home_dir: self.inner.metadata.home_dir.clone(),
            shutdown_requested: true,
        }
    }

    pub fn request_shutdown(&self) -> Result<()> {
        self.inner
            .shutdown_tx
            .send(true)
            .map_err(|_| anyhow!("failed to signal runtime shutdown"))?;
        Ok(())
    }

    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.inner.shutdown_tx.subscribe()
    }

    pub fn write_state_files(&self, config: &dyn DaemonConfig) -> Result<()> {
        let paths = daemon_paths(config);
        fs::create_dir_all(&paths.run_dir)?;
        fs::write(&paths.pid_path, format!("{}\n", self.inner.metadata.pid))?;
        fs::write(
            &paths.metadata_path,
            serde_json::to_vec_pretty(&self.inner.metadata)?,
        )?;
        Ok(())
    }

    pub fn cleanup_state_files(&self, config: &dyn DaemonConfig) -> Result<()> {
        super::cleanup_daemon_state(config)
    }
}

pub(crate) fn runtime_activity_message(summary: &RuntimeActivitySummary) -> &'static str {
    match summary.state {
        RuntimeActivityState::Idle => "runtime is healthy and idle",
        RuntimeActivityState::Waiting => "runtime is healthy and waiting",
        RuntimeActivityState::Processing => "runtime is healthy and processing work",
    }
}
