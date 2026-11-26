use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use holon_daemon::{state::merge_latest_failure, DaemonConfig};
use holon_storage::connection::is_retryable_db_error;
use sha2::{Digest, Sha256};
use tracing::info;

use crate::{
    client::ConnectionConfig,
    config::{AppConfig, ControlAuthMode, ControlTransportKind},
    host::RuntimeHost,
    runtime_db::{RuntimeDb, RuntimeDbLock, RuntimeDbRetryableError},
    startup::StartupObservation,
    storage::AppStorage,
    types::{AgentRegistryStatus, AgentVisibility, RuntimeFailureSummary},
};

use super::{RuntimeServiceHandle, RuntimeStatusResponse};

#[cfg(test)]
static PREPARE_RUNTIME_BEFORE_SERVER_HOOK: std::sync::Mutex<
    Option<Box<dyn Fn(&AppConfig) -> Result<()> + Send + Sync>>,
> = std::sync::Mutex::new(None);

/// Pre-server preparation runs before the control surface exists, so transient
/// runtime database contention can only be absorbed here. Three bounded
/// attempts keep the restart responsive; non-busy failures are never retried.
const PRE_SERVER_BUSY_RETRY_ATTEMPTS: u32 = 3;
const PRE_SERVER_BUSY_RETRY_DELAYS_MS: [u64; 2] = [100, 300];

fn prepare_with_bounded_busy_retry(
    runtime_db_path: &Path,
    mut prepare: impl FnMut() -> Result<()>,
) -> Result<()> {
    let mut attempt = 1;
    loop {
        match prepare() {
            Ok(()) => return Ok(()),
            Err(error) if is_retryable_db_error(&error) => {
                if attempt >= PRE_SERVER_BUSY_RETRY_ATTEMPTS {
                    let message = error.to_string();
                    return Err(error.context(RuntimeDbRetryableError::new(
                        "pre-server storage preparation",
                        runtime_db_path,
                        message,
                    )));
                }
                let delay_ms = PRE_SERVER_BUSY_RETRY_DELAYS_MS[(attempt - 1) as usize];
                tracing::warn!(
                    target: "holon::daemon::lifecycle",
                    attempt,
                    max_attempts = PRE_SERVER_BUSY_RETRY_ATTEMPTS,
                    delay_ms,
                    error = %error,
                    "pre-server runtime preparation hit retryable storage contention"
                );
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn prepare_runtime_before_server(config: &AppConfig) -> Result<()> {
    prepare_runtime_before_server_observed(config, &crate::startup::StartupObservation::new())
}

pub fn prepare_runtime_before_server_observed(
    config: &AppConfig,
    startup: &crate::startup::StartupObservation,
) -> Result<()> {
    let lease = RuntimeDbLock::try_lock(config.runtime_db_maintenance_lock_path())
        .context("pre-server preparation requires the daemon to be stopped")?;
    prepare_runtime_before_server_observed_with_maintenance_lease(config, startup, &lease)
}

pub fn prepare_runtime_before_server_with_maintenance_lease(
    config: &AppConfig,
    lease: &RuntimeDbLock,
) -> Result<()> {
    prepare_runtime_before_server_observed_with_maintenance_lease(
        config,
        &crate::startup::StartupObservation::new(),
        lease,
    )
}

pub fn prepare_runtime_before_server_observed_with_maintenance_lease(
    config: &AppConfig,
    startup: &crate::startup::StartupObservation,
    lease: &RuntimeDbLock,
) -> Result<()> {
    anyhow::ensure!(
        lease.path() == config.runtime_db_maintenance_lock_path(),
        "runtime maintenance lease path mismatch"
    );
    startup.observe_sync("storage_prepare", || {
        info!(
            target: "holon::daemon::lifecycle",
            startup_id = %startup.startup_id,
            home_dir = %config.home_dir.display(),
            "starting pre-server runtime preparation"
        );
        prepare_with_bounded_busy_retry(&config.runtime_db_path(), || {
            #[cfg(test)]
            if let Some(hook) = PREPARE_RUNTIME_BEFORE_SERVER_HOOK.lock().unwrap().as_ref() {
                hook(config).with_context(|| {
                    "pre-server runtime preparation failed; fix the reported storage domain error and retry"
                })?;
                return Ok(());
            }
            RuntimeHost::prepare_runtime_storage_with_maintenance_lease(config, lease).with_context(|| {
                "pre-server runtime preparation failed; fix the reported storage domain error and retry"
            })
        })?;
        info!(
            target: "holon::daemon::lifecycle",
            startup_id = %startup.startup_id,
            home_dir = %config.home_dir.display(),
            "finished pre-server runtime preparation"
        );
        Ok(())
    })
}

#[cfg(test)]
pub(super) fn set_prepare_runtime_before_server_hook(
    hook: Option<Box<dyn Fn(&AppConfig) -> Result<()> + Send + Sync>>,
) {
    *PREPARE_RUNTIME_BEFORE_SERVER_HOOK.lock().unwrap() = hook;
}

pub async fn graceful_runtime_shutdown(
    host: &RuntimeHost,
    runtime_service: &RuntimeServiceHandle,
) -> Result<()> {
    host.shutdown().await?;
    runtime_service.request_shutdown()?;
    Ok(())
}

pub fn config_fingerprint(config: &AppConfig) -> Result<String> {
    let payload = serde_json::json!({
        "default_agent_id": config.default_agent_id,
        "http_addr": config.http_addr,
        "callback_base_url": config.callback_base_url,
        "home_dir": config.home_dir,
        "socket_path": config.socket_path,
        "workspace_dir": config.workspace_dir,
        "context_window_messages": config.context_window_messages,
        "context_window_briefs": config.context_window_briefs,
        "compaction_trigger_messages": config.compaction_trigger_messages,
        "compaction_keep_recent_messages": config.compaction_keep_recent_messages,
        "prompt_budget_estimated_tokens": config.prompt_budget_estimated_tokens,
        "compaction_trigger_estimated_tokens": config.compaction_trigger_estimated_tokens,
        "compaction_keep_recent_estimated_tokens": config.compaction_keep_recent_estimated_tokens,
        "recent_episode_candidates": config.recent_episode_candidates,
        "max_relevant_episodes": config.max_relevant_episodes,
        "control_auth_mode": format!("{:?}", config.control_auth_mode),
        "control_token_configured": config.control_token.is_some(),
        "runtime_max_output_tokens": config.runtime_max_output_tokens,
        "default_tool_output_tokens": config.default_tool_output_tokens,
        "max_tool_output_tokens": config.max_tool_output_tokens,
        "disable_provider_fallback": config.provider_fallback_disabled(),
        "default_model": {
            "provider": config.default_model.provider.as_str(),
            "model": config.default_model.model,
        },
        "fallback_models": config.fallback_models.iter().map(|model| {
            serde_json::json!({
                "provider": model.provider.as_str(),
                "model": model.model,
            })
        }).collect::<Vec<_>>(),
        "providers": config.providers.iter().map(|(id, provider)| {
            serde_json::json!({
                "id": id.as_str(),
                "base_url": provider.base_url,
                "transport": provider.transport.as_str(),
                "auth": {
                    "source": provider.auth.source.as_str(),
                    "kind": provider.auth.kind.as_str(),
                    "env": provider.auth.env,
                    "profile": provider.auth.profile,
                    "external": provider.auth.external,
                    "credential_configured": provider.has_configured_credential(),
                },
            })
        }).collect::<Vec<_>>()
    });
    let encoded = serde_json::to_vec(&payload)?;
    let mut hasher = Sha256::new();
    hasher.update(encoded);
    Ok(format!("{:x}", hasher.finalize()))
}

fn latest_public_runtime_failure(config: &AppConfig) -> Result<Option<RuntimeFailureSummary>> {
    let runtime_db =
        RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
    let host_storage = AppStorage::new_global(config.home_dir.join("host"), runtime_db.clone())?;
    let mut latest = None;
    for identity in host_storage
        .latest_agent_identities()?
        .into_iter()
        .filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.visibility == AgentVisibility::Public
        })
    {
        let storage = AppStorage::new_for_agent(
            config.agent_root_dir().join(&identity.agent_id),
            identity.agent_id.clone(),
            runtime_db.clone(),
        )?;
        let failure = storage
            .read_agent()?
            .and_then(|agent| agent.last_runtime_failure);
        latest = merge_latest_failure(latest, failure);
    }
    Ok(latest)
}

pub(super) fn effective_config_mismatch_summary(
    config: &AppConfig,
    status: &RuntimeStatusResponse,
) -> String {
    let mut differences = Vec::new();
    push_diff(
        &mut differences,
        "home_dir",
        config.home_dir.display().to_string(),
        status.home_dir.display().to_string(),
    );
    push_diff(
        &mut differences,
        "socket_path",
        config.socket_path.display().to_string(),
        status.socket_path.display().to_string(),
    );
    push_diff(
        &mut differences,
        "http_addr",
        config.http_addr.clone(),
        status.http_addr.clone(),
    );

    if let Some(startup) = &status.startup_surface {
        push_diff(
            &mut differences,
            "user_home_dir",
            config
                .user_home_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".into()),
            startup
                .user_home_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".into()),
        );
        push_diff(
            &mut differences,
            "workspace_dir",
            config.workspace_dir.display().to_string(),
            startup.workspace_dir.display().to_string(),
        );
        push_diff(
            &mut differences,
            "default_agent_id",
            config.default_agent_id.clone(),
            startup.default_agent_id.clone(),
        );
        push_diff(
            &mut differences,
            "callback_base_url",
            config.callback_base_url.clone(),
            startup.callback_base_url.clone(),
        );
        push_diff(
            &mut differences,
            "control_auth_mode",
            format!("{:?}", config.control_auth_mode),
            format!("{:?}", startup.control_auth_mode),
        );
        push_diff(
            &mut differences,
            "control_token_configured",
            config.control_token.is_some().to_string(),
            startup.control_token_configured.to_string(),
        );
    } else {
        differences.push("startup_surface missing from runtime status".into());
    }

    if let Some(runtime) = &status.runtime_surface {
        push_diff(
            &mut differences,
            "model.default",
            config.default_model.as_string(),
            runtime.model_default.clone(),
        );
        push_diff(
            &mut differences,
            "model.fallbacks",
            config
                .fallback_models
                .iter()
                .map(|model| model.as_string())
                .collect::<Vec<_>>()
                .join(","),
            runtime.model_fallbacks.join(","),
        );
        push_diff(
            &mut differences,
            "runtime_max_output_tokens",
            config.runtime_max_output_tokens.to_string(),
            runtime.runtime_max_output_tokens.to_string(),
        );
        push_diff(
            &mut differences,
            "default_tool_output_tokens",
            config.default_tool_output_tokens.to_string(),
            runtime.default_tool_output_tokens.to_string(),
        );
        push_diff(
            &mut differences,
            "max_tool_output_tokens",
            config.max_tool_output_tokens.to_string(),
            runtime.max_tool_output_tokens.to_string(),
        );
        push_diff(
            &mut differences,
            "disable_provider_fallback",
            config.provider_fallback_disabled().to_string(),
            runtime.disable_provider_fallback.to_string(),
        );
    } else {
        differences.push("runtime_surface missing from runtime status".into());
    }

    if differences.is_empty() {
        "fingerprint differed, but no field-level difference was available from runtime status"
            .into()
    } else {
        differences.join("; ")
    }
}

fn push_diff(differences: &mut Vec<String>, key: &str, expected: String, actual: String) {
    if expected != actual {
        differences.push(format!("{key} expected={expected:?} actual={actual:?}"));
    }
}

impl DaemonConfig for AppConfig {
    fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    fn run_dir(&self) -> PathBuf {
        AppConfig::run_dir(self)
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn http_addr(&self) -> &str {
        &self.http_addr
    }

    fn connection_config(&self) -> ConnectionConfig {
        self.into()
    }

    fn control_auth_mode_env(&self) -> &'static str {
        match self.control_auth_mode {
            ControlAuthMode::Auto => "auto",
            ControlAuthMode::Required => "required",
            ControlAuthMode::Disabled => "disabled",
        }
    }

    fn required_tcp_control_token(&self) -> Option<&str> {
        if self.control_token_required(ControlTransportKind::Tcp) {
            self.control_token.as_deref()
        } else {
            None
        }
    }

    fn product_version(&self) -> &'static str {
        env!("HOLON_VERSION")
    }

    fn config_fingerprint(&self) -> Result<String> {
        config_fingerprint(self)
    }

    fn effective_config_mismatch_summary(&self, status: &RuntimeStatusResponse) -> String {
        effective_config_mismatch_summary(self, status)
    }

    fn prepare_runtime_before_server(&self, startup: &StartupObservation) -> Result<()> {
        prepare_runtime_before_server_observed(self, startup)
    }

    fn latest_public_runtime_failure(&self) -> Result<Option<RuntimeFailureSummary>> {
        latest_public_runtime_failure(self)
    }
}
