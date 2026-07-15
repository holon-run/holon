use std::{env, fs, path::PathBuf, sync::Arc};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    types::{AgentStatus, RuntimeFailureSummary},
    web::{WebProviderCapabilityMetadata, WebProviderKind},
};

use super::daemon_paths;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeServiceMetadata {
    pub pid: u32,
    pub home_dir: PathBuf,
    pub socket_path: PathBuf,
    pub http_addr: String,
    pub started_at: DateTime<Utc>,
    pub config_fingerprint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serve_args: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control_token_env_configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStatusResponse {
    pub ok: bool,
    pub healthy: bool,
    pub pid: u32,
    pub home_dir: PathBuf,
    pub socket_path: PathBuf,
    pub http_addr: String,
    pub started_at: DateTime<Utc>,
    pub config_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<RuntimeActivitySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_surface: Option<RuntimeStartupSurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_surface: Option<RuntimeConfigSurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<RuntimeFailureSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStartupSurface {
    pub home_dir: PathBuf,
    pub socket_path: PathBuf,
    pub workspace_dir: PathBuf,
    pub default_agent_id: String,
    #[serde(default)]
    pub callback_base_url: String,
    pub control_token_configured: bool,
    pub control_auth_mode: RuntimeControlAuthMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeControlAuthMode {
    Auto,
    Required,
    Disabled,
}

impl From<ControlAuthMode> for RuntimeControlAuthMode {
    fn from(value: ControlAuthMode) -> Self {
        match value {
            ControlAuthMode::Auto => Self::Auto,
            ControlAuthMode::Required => Self::Required,
            ControlAuthMode::Disabled => Self::Disabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigSurface {
    pub model_default: String,
    pub model_fallbacks: Vec<String>,
    pub vision_default: Option<String>,
    pub image_generation_default: Option<String>,
    pub model_catalog: Vec<String>,
    pub unknown_model_fallback_configured: bool,
    pub runtime_max_output_tokens: u32,
    pub default_tool_output_tokens: u32,
    pub max_tool_output_tokens: u32,
    pub disable_provider_fallback: bool,
    pub providers: Vec<RuntimeProviderSummary>,
    pub web_search: RuntimeWebSearchSummary,
    #[serde(default)]
    pub available_search_provider_kinds: Vec<RuntimeWebSearchProviderKindSummary>,
    pub web_search_providers: Vec<RuntimeWebSearchProviderSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeProviderSummary {
    pub id: String,
    pub transport: String,
    pub base_url: String,
    pub oauth_supported: bool,
    pub api_key_supported: bool,
    pub credential_source: String,
    pub credential_kind: String,
    pub credential_env: Option<String>,
    pub credential_profile: Option<String>,
    pub credential_external: Option<String>,
    pub credential_configured: bool,
    pub configured_in_config: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeWebSearchSummary {
    pub enabled: bool,
    pub builtin_provider_enabled: bool,
    pub provider: String,
    pub mode: String,
    pub providers: Vec<String>,
    pub max_results: usize,
    pub max_provider_attempts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeWebSearchProviderSummary {
    pub id: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub credential_profile: Option<String>,
    pub credential_configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeWebSearchProviderKindSummary {
    pub kind: String,
    pub capabilities: WebProviderCapabilityMetadata,
}

impl RuntimeConfigSurface {
    pub fn new(config: &AppConfig) -> Self {
        let mut model_catalog = config
            .validated_model_overrides
            .keys()
            .map(|value| value.as_string())
            .collect::<Vec<_>>();
        model_catalog.sort();
        Self {
            model_default: config.default_model.as_string(),
            model_fallbacks: config
                .fallback_models
                .iter()
                .map(|value| value.as_string())
                .collect(),
            vision_default: config.vision_model.as_ref().map(|value| value.as_string()),
            image_generation_default: config
                .image_generation_model
                .as_ref()
                .map(|value| value.as_string()),
            model_catalog,
            unknown_model_fallback_configured: config.validated_unknown_model_fallback.is_some(),
            runtime_max_output_tokens: config.runtime_max_output_tokens,
            default_tool_output_tokens: config.default_tool_output_tokens,
            max_tool_output_tokens: config.max_tool_output_tokens,
            disable_provider_fallback: config.disable_provider_fallback,
            providers: config
                .providers
                .values()
                .map(|provider| RuntimeProviderSummary {
                    id: provider.id.as_str().to_string(),
                    transport: provider.transport.as_str().to_string(),
                    base_url: provider.base_url.clone(),
                    oauth_supported: crate::auth::oauth_provider_config(provider.id.as_str())
                        .is_some(),
                    api_key_supported: provider.transport.as_str() != "openai_codex_responses",
                    credential_source: provider.auth.source.as_str().to_string(),
                    credential_kind: provider.auth.kind.as_str().to_string(),
                    credential_env: provider.auth.env.clone(),
                    credential_profile: provider.auth.profile.clone(),
                    credential_external: provider.auth.external.clone(),
                    credential_configured: provider.has_configured_credential(),
                    configured_in_config: config.stored_config.providers.contains_key(&provider.id),
                })
                .collect(),
            web_search: RuntimeWebSearchSummary {
                enabled: config.web_config.search.enabled,
                builtin_provider_enabled: config.web_config.search.builtin_provider_enabled,
                provider: config.web_config.search.provider.clone(),
                mode: config.web_config.search.mode.as_str().to_string(),
                providers: config.web_config.search.providers.clone(),
                max_results: config.web_config.search.max_results,
                max_provider_attempts: config.web_config.search.max_provider_attempts,
            },
            available_search_provider_kinds: WebProviderKind::ALL
                .iter()
                .map(|kind| RuntimeWebSearchProviderKindSummary {
                    kind: kind.as_str().to_string(),
                    capabilities: kind.capabilities(),
                })
                .collect(),
            web_search_providers: config
                .stored_config
                .web
                .providers
                .iter()
                .map(|(id, provider)| RuntimeWebSearchProviderSummary {
                    id: id.clone(),
                    kind: provider.kind.as_str().to_string(),
                    base_url: provider.base_url.clone(),
                    credential_profile: provider.credential_profile.clone(),
                    credential_configured: config
                        .web_config
                        .providers
                        .get(id)
                        .map(|provider| !provider.api_key.is_empty())
                        .unwrap_or(false),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeActivityState {
    Idle,
    Waiting,
    Processing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeActivitySummary {
    pub state: RuntimeActivityState,
    pub active_agent_count: usize,
    pub active_task_count: usize,
    pub processing_agent_count: usize,
    pub waiting_agent_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeShutdownResponse {
    pub ok: bool,
    pub pid: u32,
    pub home_dir: PathBuf,
    pub shutdown_requested: bool,
}

#[derive(Debug, Clone)]
pub struct RuntimeServiceHandle {
    inner: Arc<RuntimeServiceInner>,
}

#[derive(Debug)]
struct RuntimeServiceInner {
    metadata: RuntimeServiceMetadata,
    shutdown_tx: watch::Sender<bool>,
}

impl RuntimeServiceHandle {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(RuntimeServiceInner {
                metadata: RuntimeServiceMetadata {
                    pid: std::process::id(),
                    home_dir: config.home_dir.clone(),
                    socket_path: config.socket_path.clone(),
                    http_addr: config.http_addr.clone(),
                    started_at: Utc::now(),
                    config_fingerprint: super::config_fingerprint(config)?,
                    serve_args: env::var(super::DAEMON_SERVE_ARGS_ENV)
                        .ok()
                        .and_then(|value| serde_json::from_str(&value).ok())
                        .unwrap_or_default(),
                    control_token_env_configured: env::var_os("HOLON_CONTROL_TOKEN").is_some(),
                },
                shutdown_tx,
            }),
        })
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
            healthy: true,
            pid: self.inner.metadata.pid,
            home_dir: self.inner.metadata.home_dir.clone(),
            socket_path: self.inner.metadata.socket_path.clone(),
            http_addr: self.inner.metadata.http_addr.clone(),
            started_at: self.inner.metadata.started_at,
            config_fingerprint: self.inner.metadata.config_fingerprint.clone(),
            startup_surface: Some(startup_surface),
            runtime_surface: Some(runtime_surface),
            activity: Some(activity),
            last_failure,
        }
    }

    pub fn readiness_response(
        &self,
        startup_surface: RuntimeStartupSurface,
        runtime_surface: RuntimeConfigSurface,
    ) -> RuntimeStatusResponse {
        RuntimeStatusResponse {
            ok: true,
            healthy: true,
            pid: self.inner.metadata.pid,
            home_dir: self.inner.metadata.home_dir.clone(),
            socket_path: self.inner.metadata.socket_path.clone(),
            http_addr: self.inner.metadata.http_addr.clone(),
            started_at: self.inner.metadata.started_at,
            config_fingerprint: self.inner.metadata.config_fingerprint.clone(),
            startup_surface: Some(startup_surface),
            runtime_surface: Some(runtime_surface),
            activity: None,
            last_failure: None,
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

    pub fn write_state_files(&self, config: &AppConfig) -> Result<()> {
        let paths = daemon_paths(config);
        fs::create_dir_all(&paths.run_dir)?;
        fs::write(&paths.pid_path, format!("{}\n", self.inner.metadata.pid))?;
        fs::write(
            &paths.metadata_path,
            serde_json::to_vec_pretty(&self.inner.metadata)?,
        )?;
        Ok(())
    }

    pub fn cleanup_state_files(&self, config: &AppConfig) -> Result<()> {
        super::cleanup_daemon_state(config)
    }
}

pub async fn runtime_activity_summary(host: &RuntimeHost) -> Result<RuntimeActivitySummary> {
    let agents = host.public_agent_activity_snapshots().await?;
    let active_task_count = agents
        .iter()
        .map(|agent| agent.active_task_count)
        .sum::<usize>();
    let processing_agent_count = agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.status,
                AgentStatus::Booting | AgentStatus::AwakeRunning
            )
        })
        .count();
    let waiting_agent_count = agents
        .iter()
        .filter(|agent| {
            !matches!(
                agent.status,
                AgentStatus::Booting | AgentStatus::AwakeRunning
            ) && (agent.active_task_count > 0 || agent.status == AgentStatus::AwaitingTask)
        })
        .count();
    let state = if processing_agent_count > 0 {
        RuntimeActivityState::Processing
    } else if waiting_agent_count > 0 || active_task_count > 0 {
        RuntimeActivityState::Waiting
    } else {
        RuntimeActivityState::Idle
    };
    Ok(RuntimeActivitySummary {
        state,
        active_agent_count: agents.len(),
        active_task_count,
        processing_agent_count,
        waiting_agent_count,
    })
}

pub(crate) fn runtime_activity_message(summary: &RuntimeActivitySummary) -> &'static str {
    match summary.state {
        RuntimeActivityState::Idle => "runtime is healthy and idle",
        RuntimeActivityState::Waiting => "runtime is healthy and waiting",
        RuntimeActivityState::Processing => "runtime is healthy and processing work",
    }
}
