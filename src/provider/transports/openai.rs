use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use reqwest::{header::HeaderMap, Client, RequestBuilder, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use crate::{
    auth::{
        load_codex_cli_credential, load_codex_oauth_profile_credential,
        refresh_codex_oauth_profile_material, CodexCliCredential, CodexOAuthRefreshFailure,
    },
    config::{
        load_credential_store_at, save_credential_store_at, AppConfig, CredentialKind,
        CredentialProfileFile, CredentialSource, ModelRef, ProviderBuiltinWebSearchConfig,
        ProviderId, ProviderRuntimeConfig,
    },
    context::ContextConfig,
    model_catalog::{BuiltInModelCatalog, ModelVerbosity},
    provider::{
        builtin_web_search_probe_turn_request, emitted_tool_json_schema,
        http_trace::{ProviderHttpTrace, ProviderHttpTraceRequest},
        AgentProvider, ConversationMessage, ModelBlock, ProviderBuiltinWebSearchCapability,
        ProviderCacheUsage, ProviderGenerateImageRequest, ProviderGenerateImageResponse,
        ProviderGeneratedImage, ProviderIncrementalContinuationDiagnostics,
        ProviderNativeWebSearchDiagnostics, ProviderNativeWebSearchKind,
        ProviderNativeWebSearchRequest, ProviderOpenAiRemoteCompactionDiagnostics,
        ProviderOpenAiRequestControlsDiagnostics, ProviderPromptFrame, ProviderRequestDiagnostics,
        ProviderResponseFormatDiagnostics, ProviderResponseFormatRequest,
        ProviderTransportDiagnostics, ProviderTurnRequest, ProviderTurnResponse,
        ToolSchemaContract,
    },
    token_estimate::estimate_json_tokens,
};

use super::{build_http_client, request_send_timeout, response_body_timeout, stream_idle_timeout};
use crate::provider::retry::{
    classify_reqwest_transport_error_with_trace, classify_status_error_with_trace,
    invalid_response_error, provider_transport_error, timeout_transport_error_with_trace,
    ProviderFailureClassification, ProviderFailureKind, ProviderTransportError, RetryDisposition,
};

#[derive(Clone)]
pub struct OpenAiProvider {
    client: Client,
    provider_id: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    max_output_tokens: u32,
    reasoning_effort: Option<String>,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
    compaction_policy: OpenAiCompactionPolicy,
    trace_home_dir: PathBuf,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone)]
pub struct OpenAiCodexProvider {
    client: Client,
    provider_id: String,
    base_url: String,
    credential_profile: Option<String>,
    credential_material: Option<String>,
    credential_external: Option<String>,
    credential_store_path: Option<PathBuf>,
    codex_home: std::path::PathBuf,
    originator: String,
    model: String,
    max_output_tokens: u32,
    reasoning_effort: Option<String>,
    supports_reasoning: bool,
    verbosity: Option<ModelVerbosity>,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
    compaction_policy: OpenAiCompactionPolicy,
    trace_home_dir: PathBuf,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone)]
pub struct OpenAiChatCompletionsProvider {
    client: Client,
    provider_id: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    max_output_tokens: u32,
    trace_home_dir: PathBuf,
    continuation: Arc<Mutex<OpenAiContinuationState>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenAiResponsesTransportContract {
    StandardJson,
    CodexStreaming,
}

const OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND: &str = "responses_compact";

#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenAiCompactionPolicy {
    pub(crate) trigger_input_tokens: u64,
}

fn trace_response_headers(
    trace: Option<&ProviderHttpTraceRequest>,
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) {
    if let Some(trace) = trace {
        trace.write_response_headers(status, headers);
    }
}

fn trace_response_body(trace: Option<&ProviderHttpTraceRequest>, body: &str) {
    if let Some(trace) = trace {
        trace.write_response_body(body);
    }
}

fn trace_stream_chunk(trace: Option<&ProviderHttpTraceRequest>, chunk: &[u8]) {
    if let Some(trace) = trace {
        trace.write_stream_chunk(chunk);
    }
}

fn trace_stream_terminal(trace: Option<&ProviderHttpTraceRequest>, body: &Value) {
    if let Some(trace) = trace {
        trace.write_stream_terminal(body);
    }
}

fn request_agent_id(request: &ProviderTurnRequest) -> Option<&str> {
    request
        .prompt_frame
        .cache
        .as_ref()
        .map(|cache| cache.agent_id.as_str())
}

#[derive(Debug, Default)]
struct OpenAiContinuationState {
    windows: HashMap<OpenAiContinuationScope, OpenAiProviderWindow>,
    unsupported_compact_endpoints: HashMap<String, u16>,
    next_generation: u64,
}

fn openai_responses_url(base_url: &str) -> String {
    format!("{}/responses", base_url.trim_end_matches('/'))
}

fn openai_responses_compact_url(base_url: &str) -> String {
    format!("{}/responses/compact", base_url.trim_end_matches('/'))
}

fn openai_codex_api_base_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/codex") {
        base_url.to_string()
    } else {
        format!("{base_url}/codex")
    }
}

fn openai_codex_responses_url(base_url: &str) -> String {
    format!("{}/responses", openai_codex_api_base_url(base_url))
}

fn openai_codex_responses_compact_url(base_url: &str) -> String {
    format!("{}/responses/compact", openai_codex_api_base_url(base_url))
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct OpenAiContinuationScope {
    agent_id: String,
    prompt_cache_key: String,
}

#[derive(Debug, Clone)]
struct OpenAiProviderWindow {
    response_id: Option<String>,
    request_shape: OpenAiRequestShape,
    items: Vec<Value>,
    append_match_items: Vec<Value>,
    latest_compaction_index: Option<usize>,
    latest_input_tokens: u64,
    generation: u64,
}

#[derive(Debug, Clone)]
struct OpenAiCompactionCandidate {
    items: Vec<Value>,
    retained_tail: Vec<Value>,
    latest_compaction_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenAiRequestShape {
    wire_shape: Value,
    prompt_frame: ProviderPromptFrame,
}

#[derive(Debug)]
struct OpenAiRequestPlan {
    body: Value,
    scope: Option<OpenAiContinuationScope>,
    append_match_input: Vec<Value>,
    provider_input: Vec<Value>,
    request_shape: OpenAiRequestShape,
    diagnostics: ProviderRequestDiagnostics,
}

#[derive(Debug, Clone, Default)]
struct OpenAiContinuationMismatchDiagnostics {
    expected_prefix_items: usize,
    first_mismatch_index: Option<usize>,
    previous_item_type: Option<String>,
    current_item_type: Option<String>,
    previous_item_id: Option<String>,
    current_item_id: Option<String>,
    previous_item_hash: Option<String>,
    current_item_hash: Option<String>,
    request_shape_hash: Option<String>,
    first_mismatch_path: Option<String>,
    mismatch_kind: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ParsedOpenAiResponse {
    pub(crate) response: ProviderTurnResponse,
    pub(crate) response_id: Option<String>,
    pub(crate) output_items: Vec<Value>,
}

impl OpenAiProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai())
            .ok_or_else(|| anyhow::anyhow!("missing openai provider config"))?;
        Self::from_runtime_config_with_compaction_policy(
            provider_config,
            model,
            config.runtime_max_output_tokens,
            &config.home_dir,
            openai_compaction_policy_from_config(config, ProviderId::openai(), model),
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        Self::from_runtime_config_with_compaction_policy(
            provider_config,
            model,
            max_output_tokens,
            trace_home_dir,
            openai_compaction_policy_for_model(ProviderId::openai(), model, max_output_tokens),
        )
    }

    pub(crate) fn from_runtime_config_with_compaction_policy(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        compaction_policy: OpenAiCompactionPolicy,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let api_key = match (provider_config.auth.source, provider_config.auth.kind) {
            (CredentialSource::None, CredentialKind::None) => None,
            _ => Some(
                provider_config
                    .credential
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        let credential_name = provider_config
                            .auth
                            .env
                            .as_deref()
                            .or(provider_config.auth.profile.as_deref())
                            .or(provider_config.auth.external.as_deref())
                            .unwrap_or("configured credential");
                        anyhow::anyhow!("missing {credential_name}")
                    })?,
            ),
        };
        Ok(Self {
            client,
            provider_id: provider_config.id.as_str().to_string(),
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            api_key,
            model: model.to_string(),
            max_output_tokens,
            reasoning_effort: provider_config.reasoning_effort.clone(),
            builtin_web_search: provider_config.builtin_web_search.clone(),
            compaction_policy,
            trace_home_dir: trace_home_dir.to_path_buf(),
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }
}

impl OpenAiCodexProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai_codex())
            .ok_or_else(|| anyhow::anyhow!("missing openai-codex provider config"))?;
        Self::from_runtime_config_with_compaction_policy(
            provider_config,
            model,
            config.runtime_max_output_tokens,
            &config.home_dir,
            openai_compaction_policy_from_config(config, ProviderId::openai_codex(), model),
            openai_verbosity_from_config(config, ProviderId::openai_codex(), model),
            false,
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        supports_reasoning: bool,
    ) -> Result<Self> {
        Self::from_runtime_config_with_compaction_policy(
            provider_config,
            model,
            max_output_tokens,
            trace_home_dir,
            openai_compaction_policy_for_model(
                ProviderId::openai_codex(),
                model,
                max_output_tokens,
            ),
            openai_verbosity_for_model(ProviderId::openai_codex(), model, max_output_tokens),
            supports_reasoning,
        )
    }

    pub(crate) fn from_runtime_config_with_compaction_policy(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        compaction_policy: OpenAiCompactionPolicy,
        verbosity: Option<ModelVerbosity>,
        supports_reasoning: bool,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let codex_home = provider_config
            .codex_home
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing codex_home for OpenAI Codex provider"))?;
        resolve_openai_codex_credential(provider_config, &codex_home)?;
        Ok(Self {
            client,
            provider_id: provider_config.id.as_str().to_string(),
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            credential_profile: provider_config.auth.profile.clone(),
            credential_material: provider_config.credential.clone(),
            credential_external: provider_config.auth.external.clone(),
            credential_store_path: provider_config.credential_store_path.clone(),
            codex_home,
            originator: provider_config
                .originator
                .clone()
                .unwrap_or_else(|| "codex_cli_rs".into()),
            model: model.to_string(),
            max_output_tokens,
            reasoning_effort: provider_config.reasoning_effort.clone(),
            supports_reasoning,
            verbosity,
            builtin_web_search: provider_config.builtin_web_search.clone(),
            compaction_policy,
            trace_home_dir: trace_home_dir.to_path_buf(),
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }

    fn resolve_credential(&self) -> Result<CodexCliCredential> {
        let profile = self
            .credential_material
            .as_deref()
            .filter(|material| !material.trim().is_empty())
            .map(|material| {
                load_codex_oauth_profile_credential(
                    material,
                    self.credential_profile.as_deref().unwrap_or("openai-codex"),
                )
            })
            .transpose()?;
        let cli = if self.credential_external.as_deref() == Some("codex_cli") || profile.is_none() {
            load_codex_cli_credential(&self.codex_home).ok()
        } else {
            None
        };
        choose_openai_codex_credential(profile, cli).ok_or_else(|| {
            anyhow::anyhow!(
                "no Holon openai-codex credential profile or usable Codex CLI credentials are available"
            )
        })
    }

    async fn resolve_fresh_credential(&self) -> Result<CodexCliCredential> {
        let credential = self.resolve_credential()?;
        if !credential.source.starts_with("credential_profile:") {
            return Ok(credential);
        }
        if !credential_needs_refresh(&credential) {
            return Ok(credential);
        }
        self.refresh_holon_oauth_profile(false).await
    }

    async fn refresh_holon_oauth_profile(&self, force: bool) -> Result<CodexCliCredential> {
        let profile = self.credential_profile.as_deref().unwrap_or("openai-codex");
        let store_path = self.credential_store_path.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "OpenAI Codex Holon-managed OAuth profile {profile} cannot refresh because the credential store path is unavailable"
            )
        })?;
        let lock_path = store_path.with_extension("json.lock");
        let _lock = CredentialStoreRefreshLock::acquire(&lock_path)?;
        let mut store = load_credential_store_at(store_path)?;
        let entry = store.profiles.get(profile).cloned().ok_or_else(|| {
            anyhow::anyhow!("Holon credential profile {profile} disappeared before refresh")
        })?;
        if entry.kind != CredentialKind::OAuth {
            anyhow::bail!(
                "Holon credential profile {profile} has kind {}, but OpenAI Codex refresh requires oauth",
                entry.kind.as_str()
            );
        }
        let current = load_codex_oauth_profile_credential(&entry.material, profile)?;
        if !force && !credential_needs_refresh(&current) {
            return Ok(current);
        }
        let refreshed =
            refresh_codex_oauth_profile_material(&self.client, &entry.material, profile)
                .await
                .map_err(|failure| codex_refresh_error(profile, failure))?;
        store.profiles.insert(
            profile.to_string(),
            CredentialProfileFile {
                kind: CredentialKind::OAuth,
                material: refreshed.material,
            },
        );
        save_credential_store_at(store_path, &store)?;
        Ok(refreshed.credential)
    }

    async fn refresh_after_auth_failure(
        &self,
        credential: &CodexCliCredential,
    ) -> Result<Option<CodexCliCredential>> {
        if !credential.source.starts_with("credential_profile:") {
            return Ok(None);
        }
        self.refresh_holon_oauth_profile(true).await.map(Some)
    }
}

struct CredentialStoreRefreshLock {
    path: PathBuf,
}

impl CredentialStoreRefreshLock {
    fn acquire(path: &Path) -> Result<Self> {
        match Self::try_acquire(path) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create credential refresh lock {}",
                        path.display()
                    )
                });
            }
        }
        if Self::is_stale(path)? {
            std::fs::remove_file(path).with_context(|| {
                format!(
                    "failed to remove stale credential refresh lock {}",
                    path.display()
                )
            })?;
            return Self::try_acquire(path).map_err(|error| Self::acquire_error(path, error));
        }
        Self::try_acquire(path).map_err(|error| Self::acquire_error(path, error))
    }

    fn try_acquire(path: &Path) -> std::io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).map(|_| Self {
            path: path.to_path_buf(),
        })
    }

    fn acquire_error(path: &Path, error: std::io::Error) -> anyhow::Error {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!(
                "OpenAI Codex OAuth refresh is already in progress for this credential store; retry shortly"
            )
        } else {
            anyhow::Error::new(error).context(format!(
                "failed to create credential refresh lock {}",
                path.display()
            ))
        }
    }

    fn is_stale(path: &Path) -> Result<bool> {
        const STALE_LOCK_AFTER: Duration = Duration::from_secs(10 * 60);
        match std::fs::OpenOptions::new()
            .read(true)
            .open(path)
            .and_then(|file| file.metadata())
        {
            Ok(metadata) => Ok(metadata
                .modified()
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .map(|elapsed| elapsed >= STALE_LOCK_AFTER)
                .unwrap_or(false)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to inspect credential refresh lock {}",
                    path.display()
                )
            }),
        }
    }
}

impl Drop for CredentialStoreRefreshLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn credential_needs_refresh(credential: &CodexCliCredential) -> bool {
    credential
        .expires_at
        .map(|expires_at| expires_at <= Utc::now() + chrono::Duration::minutes(5))
        .unwrap_or(false)
}

fn choose_openai_codex_credential(
    profile: Option<CodexCliCredential>,
    cli: Option<CodexCliCredential>,
) -> Option<CodexCliCredential> {
    match (profile, cli) {
        (Some(profile), Some(cli)) => Some(
            [profile, cli]
                .into_iter()
                .max_by_key(openai_codex_credential_freshness_key)
                .expect("credential candidate array is non-empty"),
        ),
        (Some(profile), None) => Some(profile),
        (None, Some(cli)) => Some(cli),
        (None, None) => None,
    }
}

fn openai_codex_credential_freshness_key(
    credential: &CodexCliCredential,
) -> (
    Option<chrono::DateTime<Utc>>,
    Option<chrono::DateTime<Utc>>,
    u8,
) {
    let source_rank = if credential.source == "keychain" {
        3
    } else if credential.source == "file" {
        2
    } else if credential.source.starts_with("credential_profile:") {
        1
    } else {
        0
    };
    (credential.expires_at, credential.refreshed_at, source_rank)
}

fn openai_codex_headers(
    credential: &CodexCliCredential,
    originator: &str,
) -> Vec<(&'static str, String)> {
    vec![
        (
            "authorization",
            format!("Bearer {}", credential.access_token),
        ),
        ("chatgpt-account-id", credential.account_id.clone()),
        ("OpenAI-Beta", "responses=experimental".to_string()),
        ("originator", originator.to_string()),
    ]
}

fn is_openai_codex_auth_status_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ProviderTransportError>()
        .is_some_and(|error| {
            error.classification.kind == ProviderFailureKind::AuthError
                && matches!(error.status, Some(401 | 403))
        })
}

fn codex_refresh_error(profile: &str, failure: CodexOAuthRefreshFailure) -> anyhow::Error {
    anyhow::anyhow!(
        "OpenAI Codex OAuth refresh failed for Holon credential profile {profile}: {} ({})",
        failure.message,
        failure.kind.as_str()
    )
}

fn resolve_openai_codex_credential(
    provider_config: &ProviderRuntimeConfig,
    codex_home: &Path,
) -> Result<CodexCliCredential> {
    let profile = provider_config
        .credential
        .as_deref()
        .filter(|material| !material.trim().is_empty())
        .map(|material| {
            load_codex_oauth_profile_credential(
                material,
                provider_config
                    .auth
                    .profile
                    .as_deref()
                    .unwrap_or("openai-codex"),
            )
        })
        .transpose()?;
    let cli = if provider_config.auth.external.as_deref() == Some("codex_cli") || profile.is_none()
    {
        load_codex_cli_credential(codex_home).ok()
    } else {
        None
    };
    choose_openai_codex_credential(profile, cli).ok_or_else(|| {
        anyhow::anyhow!(
            "no Holon openai-codex credential profile or usable Codex CLI credentials are available"
        )
    })
}

fn openai_compaction_policy_from_config(
    config: &AppConfig,
    provider: ProviderId,
    model: &str,
) -> OpenAiCompactionPolicy {
    let policy = openai_model_policy_from_config(config, provider, model);
    OpenAiCompactionPolicy {
        trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
    }
}

fn openai_verbosity_from_config(
    config: &AppConfig,
    provider: ProviderId,
    model: &str,
) -> Option<ModelVerbosity> {
    openai_model_policy_from_config(config, provider, model).verbosity
}

fn openai_model_policy_from_config(
    config: &AppConfig,
    provider: ProviderId,
    model: &str,
) -> crate::model_catalog::ResolvedRuntimeModelPolicy {
    let base_context_config = ContextConfig {
        recent_messages: config.context_window_messages,
        recent_briefs: config.context_window_briefs,
        compaction_trigger_messages: config.compaction_trigger_messages,
        compaction_keep_recent_messages: config.compaction_keep_recent_messages,
        prompt_budget_estimated_tokens: config.prompt_budget_estimated_tokens,
        compaction_trigger_estimated_tokens: config.compaction_trigger_estimated_tokens,
        compaction_keep_recent_estimated_tokens: config.compaction_keep_recent_estimated_tokens,
        recent_episode_candidates: config.recent_episode_candidates,
        max_relevant_episodes: config.max_relevant_episodes,
        ..ContextConfig::default()
    };
    BuiltInModelCatalog::default().resolve_policy(
        &ModelRef::new(provider, model),
        &config.validated_model_overrides,
        &config.model_discovery_cache.models(),
        config.validated_unknown_model_fallback.as_ref(),
        &base_context_config,
        config.runtime_max_output_tokens,
    )
}

fn openai_compaction_policy_for_model(
    provider: ProviderId,
    model: &str,
    max_output_tokens: u32,
) -> OpenAiCompactionPolicy {
    let policy = BuiltInModelCatalog::default().resolve_policy(
        &ModelRef::new(provider, model),
        &Default::default(),
        &Default::default(),
        None,
        &ContextConfig::default(),
        max_output_tokens,
    );
    OpenAiCompactionPolicy {
        trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
    }
}

fn openai_verbosity_for_model(
    provider: ProviderId,
    model: &str,
    max_output_tokens: u32,
) -> Option<ModelVerbosity> {
    BuiltInModelCatalog::default()
        .resolve_policy(
            &ModelRef::new(provider, model),
            &Default::default(),
            &Default::default(),
            None,
            &ContextConfig::default(),
            max_output_tokens,
        )
        .verbosity
}

impl OpenAiChatCompletionsProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai())
            .ok_or_else(|| anyhow::anyhow!("missing openai provider config"))?;
        Self::from_runtime_config(
            provider_config,
            model,
            config.runtime_max_output_tokens,
            &config.home_dir,
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let api_key = match (provider_config.auth.source, provider_config.auth.kind) {
            (CredentialSource::None, CredentialKind::None) => None,
            _ => Some(
                provider_config
                    .credential
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        let credential_name = provider_config
                            .auth
                            .env
                            .as_deref()
                            .or(provider_config.auth.profile.as_deref())
                            .or(provider_config.auth.external.as_deref())
                            .unwrap_or("configured credential");
                        anyhow::anyhow!("missing {credential_name}")
                    })?,
            ),
        };
        Ok(Self {
            client,
            provider_id: provider_config.id.as_str().to_string(),
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            api_key,
            model: model.to_string(),
            max_output_tokens,
            trace_home_dir: trace_home_dir.to_path_buf(),
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }
}

#[async_trait]
impl AgentProvider for OpenAiProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let body = build_openai_responses_request(
            &self.model,
            self.max_output_tokens,
            &request,
            OpenAiResponsesTransportContract::StandardJson,
            ToolSchemaContract::Relaxed,
            self.reasoning_effort.as_deref(),
            None,
        )?;
        let mut plan = plan_openai_responses_request(body, &request, &self.continuation, true)?;
        let mut sent_diagnostics = plan.diagnostics.clone();
        let plan_scope = plan.scope.clone();
        let plan_request_shape = plan.request_shape.clone();
        let headers = self
            .api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default();
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        if let Some(remote_compaction) = maybe_compact_openai_request_plan(
            &self.continuation,
            &mut plan,
            self.compaction_policy,
            &self.client,
            openai_responses_compact_url(&self.base_url),
            headers.clone(),
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            sent_diagnostics.openai_remote_compaction = Some(remote_compaction);
            sent_diagnostics.request_lowering_mode = plan.diagnostics.request_lowering_mode.clone();
        }
        let parsed = match send_openai_responses_request(
            &self.client,
            openai_responses_url(&self.base_url),
            plan.body,
            headers.clone(),
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                return Err(error);
            }
        };
        update_openai_continuation(
            &self.continuation,
            plan_scope.clone(),
            plan_request_shape.clone(),
            plan.append_match_input,
            plan.provider_input,
            &parsed,
        );
        if sent_diagnostics.openai_remote_compaction.is_none() {
            sent_diagnostics.openai_remote_compaction = maybe_compact_openai_provider_window(
                &self.continuation,
                plan_scope.as_ref(),
                &plan_request_shape,
                self.compaction_policy,
                &self.client,
                openai_responses_compact_url(&self.base_url),
                headers,
                trace.as_ref(),
                request_agent_id(&request),
            )
            .await;
        }
        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        let body = build_openai_images_request(&self.model, &request);
        let headers = self
            .api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default();
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        let images = send_openai_images_request(
            &self.client,
            openai_images_generations_url(&self.base_url),
            body,
            headers,
            trace.as_ref(),
            None,
        )
        .await?;
        Ok(ProviderGenerateImageResponse {
            provider: self.provider_id.clone(),
            model: self.model.clone(),
            images,
        })
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
            crate::provider::ProviderPromptCapability::IncrementalResponses,
        ]
    }

    fn supports_freeform_grammar_tools(&self) -> bool {
        true
    }

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        let config = self.builtin_web_search.as_ref()?;
        Some(ProviderBuiltinWebSearchCapability {
            kind: config.kind,
            provider_id: self.provider_id.clone(),
            provider_model_ref: format!("{}/{}", self.provider_id, self.model),
            provider_transport: "openai_responses".into(),
            provider_base_url: self.base_url.clone(),
            advertised_tool_type: config.advertised_tool_type.clone(),
            backend_kind: config.backend_kind.clone(),
        })
    }

    async fn probe_builtin_web_search(
        &self,
        request: ProviderNativeWebSearchRequest,
    ) -> Result<()> {
        let probe_request = builtin_web_search_probe_turn_request(request);
        let body = build_openai_responses_request(
            &self.model,
            16,
            &probe_request,
            OpenAiResponsesTransportContract::StandardJson,
            ToolSchemaContract::Relaxed,
            self.reasoning_effort.as_deref(),
            None,
        )?;
        let headers = self
            .api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default();
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        send_openai_responses_request(
            &self.client,
            openai_responses_url(&self.base_url),
            body,
            headers,
            trace.as_ref(),
            None,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl AgentProvider for OpenAiCodexProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let model_ref = format!("{}/{}", self.provider_id, self.model);
        let credential = self.resolve_fresh_credential().await.map_err(|error| {
            openai_codex_auth_error(
                "credential_resolution",
                &model_ref,
                vec![
                    error.to_string(),
                    "OpenAI Codex can use a Holon-managed openai-codex credential profile or the external Codex CLI credential store. Holon may refresh its own profile, but only reads Codex CLI credentials and never overwrites them."
                        .into(),
                ],
                "OpenAI Codex authentication failed: no Holon openai-codex credential profile or usable Codex CLI credentials are available.",
            )
        })?;
        if let Some(expires_at) = credential.expires_at {
            if expires_at <= Utc::now() + chrono::Duration::seconds(60) {
                return Err(openai_codex_auth_error(
                    "credential_expired",
                    &model_ref,
                    vec![
                        format!(
                            "OpenAI Codex OAuth access token is expired: credential source {} expired at {}",
                            credential.source,
                            expires_at.to_rfc3339()
                        ),
                        "Run Holon onboarding login for openai-codex to configure or refresh Holon-managed OAuth, or run `codex login` if intentionally using external CLI credentials.".into(),
                    ],
                    "OpenAI Codex authentication failed: OAuth access token is expired. Run Holon onboarding login for openai-codex again, or run `codex login` if intentionally using external CLI credentials.",
                ));
            }
        }
        let body = build_openai_responses_request(
            &self.model,
            self.max_output_tokens,
            &request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
            if self.supports_reasoning {
                self.reasoning_effort.as_deref()
            } else {
                None
            },
            self.verbosity,
        )?;
        let mut plan = plan_openai_responses_request(body, &request, &self.continuation, false)?;
        let mut sent_diagnostics = plan.diagnostics.clone();
        let plan_scope = plan.scope.clone();
        let plan_request_shape = plan.request_shape.clone();
        let mut credential = credential;
        let mut headers = openai_codex_headers(&credential, &self.originator);
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        if let Some(remote_compaction) = maybe_compact_openai_request_plan(
            &self.continuation,
            &mut plan,
            self.compaction_policy,
            &self.client,
            openai_codex_responses_compact_url(&self.base_url),
            headers.clone(),
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            sent_diagnostics.openai_remote_compaction = Some(remote_compaction);
            sent_diagnostics.request_lowering_mode = plan.diagnostics.request_lowering_mode.clone();
        }
        let parsed = match send_openai_responses_streaming_request(
            &self.client,
            openai_codex_responses_url(&self.base_url),
            plan.body.clone(),
            headers.clone(),
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                if is_openai_codex_auth_status_error(&error) {
                    if let Some(refreshed) =
                        self.refresh_after_auth_failure(&credential).await.map_err(|refresh_error| {
                            invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                            openai_codex_auth_error(
                                "credential_refresh_after_auth_failure",
                                &model_ref,
                                vec![
                                    error.to_string(),
                                    refresh_error.to_string(),
                                    "OpenAI Codex returned an auth error; Holon attempted to refresh the Holon-managed profile before retrying.".into(),
                                ],
                                "OpenAI Codex authentication failed and Holon-managed OAuth refresh did not recover it.",
                            )
                        })?
                    {
                        credential = refreshed;
                        headers = openai_codex_headers(&credential, &self.originator);
                        match send_openai_responses_streaming_request(
                            &self.client,
                            openai_codex_responses_url(&self.base_url),
                            plan.body.clone(),
                            headers.clone(),
                            trace.as_ref(),
                            request_agent_id(&request),
                        )
                        .await
                        {
                            Ok(parsed) => parsed,
                            Err(retry_error) => {
                                invalidate_openai_continuation(
                                    &self.continuation,
                                    plan.scope.as_ref(),
                                );
                                return Err(retry_error);
                            }
                        }
                    } else {
                        invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                        return Err(error);
                    }
                } else {
                    invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                    return Err(error);
                }
            }
        };
        update_openai_continuation(
            &self.continuation,
            plan_scope.clone(),
            plan_request_shape.clone(),
            plan.append_match_input,
            plan.provider_input,
            &parsed,
        );
        if sent_diagnostics.openai_remote_compaction.is_none() {
            sent_diagnostics.openai_remote_compaction = maybe_compact_openai_provider_window(
                &self.continuation,
                plan_scope.as_ref(),
                &plan_request_shape,
                self.compaction_policy,
                &self.client,
                openai_codex_responses_compact_url(&self.base_url),
                headers,
                trace.as_ref(),
                request_agent_id(&request),
            )
            .await;
        }
        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        let model_ref = format!("{}/{}", self.provider_id, self.model);
        let credential = self.resolve_fresh_credential().await.map_err(|error| {
            openai_codex_auth_error(
                "credential_resolution",
                &model_ref,
                vec![
                    error.to_string(),
                    "OpenAI Codex image generation uses Codex OAuth credentials through the Responses image_generation tool.".into(),
                ],
                "OpenAI Codex image generation authentication failed: no usable OAuth credentials are available.",
            )
        })?;
        if let Some(expires_at) = credential.expires_at {
            if expires_at <= Utc::now() + chrono::Duration::seconds(60) {
                return Err(openai_codex_auth_error(
                    "credential_expired",
                    &model_ref,
                    vec![
                        format!(
                            "OpenAI Codex OAuth access token is expired: credential source {} expired at {}",
                            credential.source,
                            expires_at.to_rfc3339()
                        ),
                        "Run Holon onboarding login for openai-codex to configure or refresh Holon-managed OAuth, or run `codex login` if intentionally using external CLI credentials.".into(),
                    ],
                    "OpenAI Codex image generation authentication failed: OAuth access token is expired.",
                ));
            }
        }

        let body = build_openai_codex_image_generation_request(&self.model, &request);
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        let mut credential = credential;
        let mut headers = openai_codex_headers(&credential, &self.originator);
        let images = match send_openai_codex_image_generation_request(
            &self.client,
            openai_codex_responses_url(&self.base_url),
            body.clone(),
            headers.clone(),
            trace.as_ref(),
            None,
        )
        .await
        {
            Ok(images) => images,
            Err(error) => {
                if is_openai_codex_auth_status_error(&error) {
                    if let Some(refreshed) = self.refresh_after_auth_failure(&credential).await.map_err(|refresh_error| {
                        openai_codex_auth_error(
                            "credential_refresh_after_auth_failure",
                            &model_ref,
                            vec![
                                error.to_string(),
                                refresh_error.to_string(),
                                "OpenAI Codex returned an auth error for image generation; Holon attempted to refresh the Holon-managed profile before retrying.".into(),
                            ],
                            "OpenAI Codex image generation authentication failed and Holon-managed OAuth refresh did not recover it.",
                        )
                    })? {
                        credential = refreshed;
                        headers = openai_codex_headers(&credential, &self.originator);
                        send_openai_codex_image_generation_request(
                            &self.client,
                            openai_codex_responses_url(&self.base_url),
                            body,
                            headers,
                            trace.as_ref(),
                            None,
                        )
                        .await?
                    } else {
                        return Err(error);
                    }
                } else {
                    return Err(error);
                }
            }
        };

        Ok(ProviderGenerateImageResponse {
            provider: self.provider_id.clone(),
            model: self.model.clone(),
            images,
        })
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai-codex/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
            crate::provider::ProviderPromptCapability::IncrementalResponses,
        ]
    }

    fn supports_freeform_grammar_tools(&self) -> bool {
        true
    }

    fn builtin_web_search(&self) -> Option<ProviderBuiltinWebSearchCapability> {
        let config = self.builtin_web_search.as_ref()?;
        Some(ProviderBuiltinWebSearchCapability {
            kind: config.kind,
            provider_id: self.provider_id.clone(),
            provider_model_ref: format!("{}/{}", self.provider_id, self.model),
            provider_transport: "openai_codex_responses".into(),
            provider_base_url: self.base_url.clone(),
            advertised_tool_type: config.advertised_tool_type.clone(),
            backend_kind: config.backend_kind.clone(),
        })
    }

    async fn probe_builtin_web_search(
        &self,
        request: ProviderNativeWebSearchRequest,
    ) -> Result<()> {
        let credential = self.resolve_fresh_credential().await?;
        let probe_request = builtin_web_search_probe_turn_request(request);
        let body = build_openai_responses_request(
            &self.model,
            16,
            &probe_request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
            if self.supports_reasoning {
                self.reasoning_effort.as_deref()
            } else {
                None
            },
            self.verbosity,
        )?;
        let headers = openai_codex_headers(&credential, &self.originator);
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        send_openai_responses_streaming_request(
            &self.client,
            openai_codex_responses_url(&self.base_url),
            body,
            headers,
            trace.as_ref(),
            None,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl AgentProvider for OpenAiChatCompletionsProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        // Build Chat Completions request
        let (body, plan) = plan_chat_completion_request(
            &self.model,
            self.max_output_tokens,
            &request,
            ToolSchemaContract::Relaxed,
            false, // Streaming infrastructure exists but is not currently enabled; requests are non-streaming
            &self.continuation,
        )?;
        let sent_diagnostics = plan.diagnostics.clone();
        let headers = self
            .api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default();
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());

        // Send to /v1/chat/completions endpoint
        let parsed = match send_chat_completion_request(
            &self.client,
            chat_completions_url(&self.base_url),
            body,
            headers,
            trace.as_ref(),
            request_agent_id(&request),
        )
        .await
        {
            Ok(parsed) => parsed,
            Err(error) => {
                invalidate_openai_continuation(&self.continuation, plan.scope.as_ref());
                return Err(error);
            }
        };

        update_openai_continuation(
            &self.continuation,
            plan.scope,
            plan.request_shape,
            plan.append_match_input,
            plan.provider_input,
            &parsed,
        );

        Ok(parsed.response.with_request_diagnostics(sent_diagnostics))
    }

    async fn generate_image(
        &self,
        request: ProviderGenerateImageRequest,
    ) -> Result<ProviderGenerateImageResponse> {
        let body = build_openai_images_request(&self.model, &request);
        let headers = self
            .api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default();
        let trace = ProviderHttpTrace::from_env(self.trace_home_dir.clone());
        let images = send_openai_images_request(
            &self.client,
            openai_images_generations_url(&self.base_url),
            body,
            headers,
            trace.as_ref(),
            None,
        )
        .await?;
        Ok(ProviderGenerateImageResponse {
            provider: self.provider_id.clone(),
            model: self.model.clone(),
            images,
        })
    }

    #[cfg(test)]
    fn configured_model_refs(&self) -> Vec<String> {
        vec![format!("openai/{}", self.model)]
    }

    fn prompt_capabilities(&self) -> Vec<crate::provider::ProviderPromptCapability> {
        vec![
            crate::provider::ProviderPromptCapability::FullRequestOnly,
            crate::provider::ProviderPromptCapability::PromptCacheKey,
        ]
    }
}

fn chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if has_trailing_version_segment(trimmed) {
        format!("{trimmed}/chat/completions")
    } else {
        format!("{trimmed}/v1/chat/completions")
    }
}

fn openai_images_generations_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/images/generations") {
        trimmed.to_string()
    } else if has_trailing_version_segment(trimmed) {
        format!("{trimmed}/images/generations")
    } else {
        format!("{trimmed}/v1/images/generations")
    }
}

fn build_openai_images_request(model: &str, request: &ProviderGenerateImageRequest) -> Value {
    let mut body = json!({
        "model": model,
        "prompt": request.prompt,
        "n": 1,
        "response_format": "b64_json",
    });
    if let Some(size) = request
        .size
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["size"] = Value::String(size.clone());
    }
    if let Some(background) = request
        .background
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["background"] = Value::String(background.clone());
    }
    if let Some(output_format) = request
        .output_format
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["output_format"] = Value::String(output_format.clone());
    }
    body
}

fn build_openai_codex_image_generation_request(
    model: &str,
    request: &ProviderGenerateImageRequest,
) -> Value {
    let mut body = json!({
        "model": model,
        "input": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": request.prompt,
                    }
                ],
            }
        ],
        "tools": [
            {
                "type": "image_generation",
                "output_format": request
                    .output_format
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("png"),
            }
        ],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
        "stream": true,
    });
    if let Some(size) = request
        .size
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["tools"][0]["size"] = Value::String(size.clone());
    }
    if let Some(background) = request
        .background
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        body["tools"][0]["background"] = Value::String(background.clone());
    }
    body
}

fn has_trailing_version_segment(url: &str) -> bool {
    let Some(last_segment) = url.rsplit('/').next() else {
        return false;
    };
    let Some(version_digits) = last_segment.strip_prefix('v') else {
        return false;
    };
    !version_digits.is_empty() && version_digits.chars().all(|ch| ch.is_ascii_digit())
}

impl ProviderTurnResponse {
    fn with_request_diagnostics(mut self, diagnostics: ProviderRequestDiagnostics) -> Self {
        self.request_diagnostics = Some(diagnostics);
        self
    }
}

impl ParsedOpenAiResponse {
    fn with_provider_request_id(mut self, provider_request_id: Option<String>) -> Self {
        self.response.provider_request_id = provider_request_id;
        self
    }
}

fn provider_request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("request-id"))
        .or_else(|| headers.get("openai-request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(crate) fn build_chat_completion_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    tool_schema_contract: ToolSchemaContract,
    stream: bool,
) -> Result<Value> {
    // Build messages array for Chat Completions API
    let messages =
        build_chat_completion_messages(&request.prompt_frame.system_prompt, &request.conversation)?;

    // Build tools array in OpenAI function calling format
    let tools = if !request.tools.is_empty() {
        Some(
            request
                .tools
                .iter()
                .map(|tool| {
                    Ok(json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": emitted_tool_json_schema(&tool.input_schema, tool_schema_contract)?,
                            "strict": matches!(tool_schema_contract, ToolSchemaContract::Strict),
                        }
                    }))
                })
                .collect::<Result<Vec<_>>>()?,
        )
    } else {
        None
    };

    // Build Chat Completions request body
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens,
        "stream": stream,
    });

    // Add tools if present
    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = Value::String("auto".to_string());
    }

    // Add prompt cache key if available
    if let Some(cache) = request.prompt_frame.cache.as_ref() {
        body["prompt_cache_key"] = Value::String(cache.prompt_cache_key.clone());
    }

    Ok(body)
}

fn plan_chat_completion_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    tool_schema_contract: ToolSchemaContract,
    stream: bool,
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> Result<(Value, OpenAiRequestPlan)> {
    let full_body = build_chat_completion_request(
        model,
        max_output_tokens,
        request,
        tool_schema_contract,
        stream,
    )?;

    let body_messages = full_body
        .get("messages")
        .and_then(|messages| messages.as_array())
        .cloned()
        .unwrap_or_default();

    // Calculate continuation scope
    let scope = continuation_scope(request);
    let full_messages = body_messages;
    let full_message_count = full_messages.len();
    let request_shape = request_shape_for_chat_completion(&full_body, request);

    // Check for continuation opportunity
    let Some(scope_ref) = scope.as_ref() else {
        // No continuation scope - send full request
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                scope,
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "missing_continuation_scope",
                    None,
                    full_message_count,
                    None,
                    None,
                    None,
                    response_format_diagnostics(false, request),
                ),
            },
        ));
    };

    let previous = lock_openai_continuation(continuation)
        .windows
        .get(scope_ref)
        .cloned();

    let Some(previous) = previous else {
        // No previous state - send full request
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                scope,
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "not_applicable_initial_request",
                    None,
                    full_message_count,
                    None,
                    None,
                    None,
                    response_format_diagnostics(false, request),
                ),
            },
        ));
    };

    // Check if request shape changed
    if previous.request_shape != request_shape {
        // Request changed - send full request
        let request_shape_hash = request_shape_hash(&request_shape);
        return Ok((
            full_body.clone(),
            OpenAiRequestPlan {
                body: full_body,
                scope,
                append_match_input: full_messages.clone(),
                provider_input: full_messages,
                request_shape,
                diagnostics: incremental_diagnostics(
                    "full_request",
                    "request_shape_changed",
                    None,
                    full_message_count,
                    Some(OpenAiContinuationMismatchDiagnostics {
                        request_shape_hash: Some(request_shape_hash),
                        ..OpenAiContinuationMismatchDiagnostics::default()
                    }),
                    None,
                    None,
                    response_format_diagnostics(false, request),
                ),
            },
        ));
    }

    // Chat Completions continuation currently cannot safely reconstruct an
    // assistant message from the provider window for prefix matching.
    // `full_messages` contains message objects, but `response_output` is not
    // guaranteed to store a comparable message value, so incremental
    // continuation would be unreliable here. Send the full request instead.
    let request_shape_hash = request_shape_hash(&request_shape);
    return Ok((
        full_body.clone(),
        OpenAiRequestPlan {
            body: full_body,
            scope,
            append_match_input: full_messages.clone(),
            provider_input: full_messages,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "chat_completions_incremental_continuation_unsupported",
                None,
                full_message_count,
                Some(OpenAiContinuationMismatchDiagnostics {
                    request_shape_hash: Some(request_shape_hash),
                    ..OpenAiContinuationMismatchDiagnostics::default()
                }),
                None,
                None,
                response_format_diagnostics(false, request),
            ),
        },
    ));
}

fn request_shape_for_chat_completion(
    body: &Value,
    request: &ProviderTurnRequest,
) -> OpenAiRequestShape {
    let mut wire_shape = body.clone();
    if let Some(object) = wire_shape.as_object_mut() {
        object.remove("messages");
        object.remove("prompt_cache_key");
    }
    OpenAiRequestShape {
        wire_shape,
        prompt_frame: request.prompt_frame.clone(),
    }
}

pub(crate) fn build_chat_completion_messages(
    system_prompt: &str,
    conversation: &[ConversationMessage],
) -> Result<Vec<Value>> {
    let mut messages = Vec::new();

    // Add system prompt as first message
    if !system_prompt.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": system_prompt,
        }));
    }

    // Process conversation messages
    for msg in conversation {
        match msg {
            ConversationMessage::UserText(text) => {
                messages.push(json!({
                    "role": "user",
                    "content": text,
                }));
            }
            ConversationMessage::UserBlocks(blocks) => {
                // Concatenate all block texts
                let content = blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                messages.push(json!({
                    "role": "user",
                    "content": content,
                }));
            }
            ConversationMessage::UserImage {
                prompt,
                media_type,
                data_base64,
            } => {
                messages.push(json!({
                    "role": "user",
                    "content": [
                        { "type": "text", "text": prompt },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{media_type};base64,{data_base64}"),
                            },
                        },
                    ],
                }));
            }
            ConversationMessage::AssistantBlocks(blocks) => {
                // Extract text content and tool calls
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in blocks {
                    match block {
                        ModelBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        ModelBlock::ToolUse { id, name, input } => {
                            tool_calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(input)
                                        .context("failed to serialize tool call arguments")?,
                                }
                            }));
                        }
                        ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => {}
                    }
                }

                // Build assistant message
                let content = text_parts.join("\n\n");
                let mut message = json!({
                    "role": "assistant",
                });

                // Set content field: null for tool-only messages, otherwise string content
                if !text_parts.is_empty() {
                    message["content"] = Value::String(content);
                } else {
                    message["content"] = Value::Null;
                }

                // Add tool_calls if present
                if !tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(tool_calls);
                }

                messages.push(message);
            }
            ConversationMessage::UserToolResults(results) => {
                // Each tool result becomes a separate "tool" message
                for result in results {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": result.tool_use_id,
                        "content": result.content,
                    }));
                }
            }
        }
    }

    Ok(messages)
}

pub(crate) fn build_openai_responses_request(
    model: &str,
    max_output_tokens: u32,
    request: &ProviderTurnRequest,
    contract: OpenAiResponsesTransportContract,
    tool_schema_contract: ToolSchemaContract,
    reasoning_effort: Option<&str>,
    verbosity: Option<ModelVerbosity>,
) -> Result<Value> {
    let mut tools = request
        .tools
        .iter()
        .map(|tool| {
            if let Some(grammar) = tool.freeform_grammar.as_ref() {
                Ok(json!({
                    "type": "custom",
                    "name": tool.name,
                    "description": tool.description,
                    "format": {
                        "type": "grammar",
                        "syntax": grammar.syntax,
                        "definition": grammar.definition,
                    }
                }))
            } else {
                Ok(json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": emitted_tool_json_schema(&tool.input_schema, tool_schema_contract)?,
                    "strict": matches!(tool_schema_contract, ToolSchemaContract::Strict),
                }))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(tool) = openai_native_web_search_tool(request) {
        tools.push(tool);
        tools.extend(openai_native_web_search_extra_tools(request));
    }

    let mut body = json!({
        "model": model,
        "instructions": request.prompt_frame.system_prompt,
        "input": build_openai_input(&request.conversation)?,
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
    });
    if let Some(cache) = request.prompt_frame.cache.as_ref() {
        body["prompt_cache_key"] = Value::String(cache.prompt_cache_key.clone());
    }
    if let Some(response_format) = openai_response_format(request) {
        body["text"]["format"] = response_format;
    }
    if let Some(reasoning_effort) = reasoning_effort {
        body["reasoning"] = json!({ "effort": reasoning_effort });
    }
    match contract {
        OpenAiResponsesTransportContract::StandardJson => {
            body["max_output_tokens"] = Value::from(max_output_tokens);
        }
        OpenAiResponsesTransportContract::CodexStreaming => {
            body["stream"] = Value::Bool(true);
            if let Some(verbosity) = verbosity {
                body["text"] = json!({ "verbosity": verbosity.as_str() });
            }
            if reasoning_effort.is_some() {
                body["include"] = json!(["reasoning.encrypted_content"]);
            } else {
                body["reasoning"] = Value::Null;
                body["include"] = Value::Array(Vec::new());
            }
        }
    }
    Ok(body)
}

fn openai_response_format(request: &ProviderTurnRequest) -> Option<Value> {
    match request.response_format.as_ref()? {
        ProviderResponseFormatRequest::JsonSchema(format) => Some(json!({
            "type": "json_schema",
            "name": format.name,
            "schema": format.schema,
            "strict": format.strict,
        })),
    }
}

fn openai_native_web_search_tool(request: &ProviderTurnRequest) -> Option<Value> {
    let native = request.native_web_search.as_ref()?;
    match native.kind {
        ProviderNativeWebSearchKind::OpenAi => Some(json!({ "type": native.advertised_tool_type })),
        ProviderNativeWebSearchKind::Xai => Some(json!({ "type": native.advertised_tool_type })),
        _ => None,
    }
}

fn openai_native_web_search_extra_tools(request: &ProviderTurnRequest) -> Vec<Value> {
    if request
        .native_web_search
        .as_ref()
        .is_some_and(|native| native.kind == ProviderNativeWebSearchKind::Xai)
    {
        vec![json!({ "type": "x_search" })]
    } else {
        Vec::new()
    }
}

fn openai_request_controls_diagnostics(body: &Value) -> ProviderOpenAiRequestControlsDiagnostics {
    let reasoning_effort = body
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let verbosity = body
        .get("text")
        .and_then(|text| text.get("verbosity"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let include_reasoning_encrypted_content = body
        .get("include")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str() == Some("reasoning.encrypted_content"))
        });
    let max_output_tokens_sent = body.get("max_output_tokens").is_some();
    let codex_streaming = body.get("stream").and_then(Value::as_bool) == Some(true);
    ProviderOpenAiRequestControlsDiagnostics {
        reasoning_sent: reasoning_effort.is_some(),
        reasoning_effort,
        verbosity,
        include_reasoning_encrypted_content,
        max_output_tokens_sent,
        max_output_tokens_unsupported: codex_streaming,
    }
}

fn plan_openai_responses_request(
    mut body: Value,
    request: &ProviderTurnRequest,
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    allow_previous_response_id: bool,
) -> Result<OpenAiRequestPlan> {
    let full_input = body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI request did not contain input array",
                "missing input",
            )
        })?;
    let full_input_items = full_input.len();
    let append_match_input = openai_append_match_input_items(&full_input);
    let request_shape = request_shape_without_input(&body, request);
    let scope = continuation_scope(request);
    let request_controls = Some(openai_request_controls_diagnostics(&body));
    let Some(scope_ref) = scope.as_ref() else {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            append_match_input,
            provider_input: full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "missing_continuation_scope",
                None,
                full_input_items,
                None,
                request_controls,
                native_web_search_diagnostics(request),
                response_format_diagnostics(true, request),
            ),
        });
    };
    let previous = lock_openai_continuation(continuation)
        .windows
        .get(scope_ref)
        .cloned();
    let Some(previous) = previous else {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            append_match_input,
            provider_input: full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "not_applicable_initial_request",
                None,
                full_input_items,
                None,
                request_controls,
                native_web_search_diagnostics(request),
                response_format_diagnostics(true, request),
            ),
        });
    };

    if previous.request_shape != request_shape {
        let request_shape_hash = request_shape_hash(&request_shape);
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            append_match_input,
            provider_input: full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "request_shape_changed",
                None,
                full_input_items,
                Some(OpenAiContinuationMismatchDiagnostics {
                    request_shape_hash: Some(request_shape_hash),
                    ..OpenAiContinuationMismatchDiagnostics::default()
                }),
                request_controls,
                native_web_search_diagnostics(request),
                response_format_diagnostics(true, request),
            ),
        });
    }

    let expected_prefix = previous.append_match_items.clone();
    let mismatch = openai_continuation_mismatch_diagnostics(
        &expected_prefix,
        &append_match_input,
        &request_shape,
    );
    if expected_prefix.is_empty()
        || append_match_input.len() <= expected_prefix.len()
        || !append_match_input.starts_with(&expected_prefix)
    {
        return Ok(OpenAiRequestPlan {
            body,
            scope,
            append_match_input,
            provider_input: full_input,
            request_shape,
            diagnostics: incremental_diagnostics(
                "full_request",
                "conversation_not_strict_append_only",
                None,
                full_input_items,
                Some(mismatch),
                request_controls,
                native_web_search_diagnostics(request),
                response_format_diagnostics(true, request),
            ),
        });
    }

    let incremental_input = full_input[expected_prefix.len()..].to_vec();
    let response_id = allow_previous_response_id
        .then(|| previous.response_id.clone())
        .flatten();
    let has_response_id = response_id.is_some();
    let replay_is_compacted = previous.latest_compaction_index.is_some();
    let provider_input = if let Some(response_id) = response_id {
        body["input"] = Value::Array(incremental_input.clone());
        body["previous_response_id"] = Value::String(response_id);
        incremental_input.clone()
    } else {
        let mut provider_input = previous.items.clone();
        provider_input.extend(incremental_input.clone());
        body["input"] = Value::Array(provider_input.clone());
        provider_input
    };
    let request_shape_hash = request_shape_hash(&request_shape);
    Ok(OpenAiRequestPlan {
        body,
        scope,
        append_match_input,
        provider_input,
        request_shape,
        diagnostics: ProviderRequestDiagnostics {
            request_lowering_mode: openai_append_match_lowering_mode(
                has_response_id,
                replay_is_compacted,
            ),
            anthropic_cache: None,
            anthropic_context_management: None,
            openai_request_controls: request_controls,
            openai_remote_compaction: None,
            incremental_continuation: Some(ProviderIncrementalContinuationDiagnostics {
                status: "hit".into(),
                fallback_reason: None,
                incremental_input_items: Some(incremental_input.len()),
                full_input_items: Some(full_input_items),
                expected_prefix_items: Some(expected_prefix.len()),
                first_mismatch_index: None,
                previous_item_type: None,
                current_item_type: None,
                previous_item_id: None,
                current_item_id: None,
                previous_item_hash: None,
                current_item_hash: None,
                request_shape_hash: Some(request_shape_hash),
                first_mismatch_path: None,
                mismatch_kind: None,
            }),
            native_web_search: native_web_search_diagnostics(request),
            response_format: response_format_diagnostics(true, request),
        },
    })
}

fn openai_append_match_lowering_mode(has_response_id: bool, replay_is_compacted: bool) -> String {
    if has_response_id {
        "incremental_continuation".into()
    } else if replay_is_compacted {
        "provider_window_compacted".into()
    } else {
        "provider_window_replay".into()
    }
}

fn continuation_scope(request: &ProviderTurnRequest) -> Option<OpenAiContinuationScope> {
    request
        .prompt_frame
        .cache
        .as_ref()
        .map(|cache| OpenAiContinuationScope {
            agent_id: cache.agent_id.clone(),
            prompt_cache_key: cache.prompt_cache_key.clone(),
        })
}

fn request_shape_without_input(body: &Value, request: &ProviderTurnRequest) -> OpenAiRequestShape {
    let mut wire_shape = body.clone();
    if let Some(object) = wire_shape.as_object_mut() {
        object.remove("input");
        object.remove("previous_response_id");
    }
    OpenAiRequestShape {
        wire_shape,
        prompt_frame: request.prompt_frame.clone(),
    }
}

fn openai_continuation_mismatch_diagnostics(
    expected_prefix: &[Value],
    full_input: &[Value],
    request_shape: &OpenAiRequestShape,
) -> OpenAiContinuationMismatchDiagnostics {
    let first_mismatch_index = expected_prefix
        .iter()
        .zip(full_input.iter())
        .position(|(previous, current)| previous != current)
        .or_else(|| {
            (expected_prefix.len() != full_input.len())
                .then_some(expected_prefix.len().min(full_input.len()))
        });
    let previous = first_mismatch_index.and_then(|index| expected_prefix.get(index));
    let current = first_mismatch_index.and_then(|index| full_input.get(index));
    let item_path = match (first_mismatch_index, previous, current) {
        (Some(index), Some(previous), Some(current)) => {
            let suffix = first_json_mismatch_path(previous, current).unwrap_or_default();
            Some(format!("/{index}{suffix}"))
        }
        (Some(index), _, _) => Some(format!("/{index}")),
        _ => None,
    };
    OpenAiContinuationMismatchDiagnostics {
        expected_prefix_items: expected_prefix.len(),
        first_mismatch_index,
        previous_item_type: previous.map(openai_item_type),
        current_item_type: current.map(openai_item_type),
        previous_item_id: previous.and_then(openai_item_stable_id),
        current_item_id: current.and_then(openai_item_stable_id),
        previous_item_hash: previous.map(openai_item_hash),
        current_item_hash: current.map(openai_item_hash),
        request_shape_hash: Some(request_shape_hash(request_shape)),
        first_mismatch_path: item_path.clone(),
        mismatch_kind: Some(openai_mismatch_kind(
            previous,
            current,
            item_path.as_deref(),
        )),
    }
}

fn first_json_mismatch_path(previous: &Value, current: &Value) -> Option<String> {
    if previous == current {
        return None;
    }
    match (previous, current) {
        (Value::Array(previous), Value::Array(current)) => {
            let shared = previous.len().min(current.len());
            for index in 0..shared {
                if let Some(path) = first_json_mismatch_path(&previous[index], &current[index]) {
                    return Some(format!("/{index}{path}"));
                }
            }
            Some(format!("/{shared}"))
        }
        (Value::Object(previous), Value::Object(current)) => {
            let keys = previous
                .keys()
                .chain(current.keys())
                .collect::<std::collections::BTreeSet<_>>();
            for key in keys {
                match (previous.get(key), current.get(key)) {
                    (Some(previous), Some(current)) => {
                        if let Some(path) = first_json_mismatch_path(previous, current) {
                            return Some(format!("/{}{}", json_pointer_escape(key), path));
                        }
                    }
                    _ => return Some(format!("/{}", json_pointer_escape(key))),
                }
            }
            Some(String::new())
        }
        _ => Some(String::new()),
    }
}

fn json_pointer_escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn openai_mismatch_kind(
    previous: Option<&Value>,
    current: Option<&Value>,
    path: Option<&str>,
) -> String {
    let Some(previous) = previous else {
        return "length_mismatch".into();
    };
    let Some(current) = current else {
        return "length_mismatch".into();
    };
    let previous_type = openai_item_type(previous);
    let current_type = openai_item_type(current);
    if previous_type != current_type {
        return "semantic_mismatch".into();
    }
    let path = path.unwrap_or_default();
    if path.contains("/id")
        || path.contains("/status")
        || path.contains("/metadata")
        || path.contains("/annotations")
        || path.contains("/logprobs")
    {
        return "provider_metadata_only".into();
    }
    match previous_type.as_str() {
        "message" => {
            if previous.get("role").and_then(Value::as_str) == Some("assistant")
                && path.contains("/content")
                && !path.ends_with("/text")
            {
                "assistant_text_shape".into()
            } else {
                "semantic_mismatch".into()
            }
        }
        "function_call" | "custom_tool_call" => "tool_call_shape".into(),
        "function_call_output" | "custom_tool_call_output" => "tool_result_shape".into(),
        _ => "semantic_mismatch".into(),
    }
}

fn openai_item_type(item: &Value) -> String {
    item.get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| match item {
            Value::Object(_) => "object",
            Value::Array(_) => "array",
            Value::String(_) => "string",
            Value::Number(_) => "number",
            Value::Bool(_) => "bool",
            Value::Null => "null",
        })
        .to_string()
}

fn openai_item_stable_id(item: &Value) -> Option<String> {
    if let Some(id) = ["id", "call_id"]
        .into_iter()
        .find_map(|key| item.get(key).and_then(Value::as_str))
    {
        return Some(id.to_string());
    }
    item.get("role").and_then(Value::as_str).map(|role| {
        let item_type = openai_item_type(item);
        format!("{item_type}:{role}")
    })
}

fn openai_item_hash(item: &Value) -> String {
    sha256_hex(canonical_json(item).as_bytes())
}

fn latest_openai_compaction_index(items: &[Value]) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| openai_is_compaction_item(item).then_some(index))
}

fn openai_is_compaction_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("compaction" | "compaction_summary")
    )
}

fn request_shape_hash(request_shape: &OpenAiRequestShape) -> String {
    let value = json!({
        "wire_shape": request_shape.wire_shape,
        "prompt_frame": request_shape.prompt_frame,
    });
    sha256_hex(canonical_json(&value).as_bytes())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into()),
        Value::Array(values) => {
            let items = values.iter().map(canonical_json).collect::<Vec<_>>();
            format!("[{}]", items.join(","))
        }
        Value::Object(map) => {
            let ordered = map
                .iter()
                .map(|(key, value)| (key, value))
                .collect::<BTreeMap<_, _>>();
            let items = ordered
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", items.join(","))
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn incremental_diagnostics(
    request_lowering_mode: &str,
    fallback_reason: &str,
    incremental_input_items: Option<usize>,
    full_input_items: usize,
    mismatch: Option<OpenAiContinuationMismatchDiagnostics>,
    openai_request_controls: Option<ProviderOpenAiRequestControlsDiagnostics>,
    native_web_search: Option<ProviderNativeWebSearchDiagnostics>,
    response_format: Option<ProviderResponseFormatDiagnostics>,
) -> ProviderRequestDiagnostics {
    let mismatch = mismatch.unwrap_or_default();
    ProviderRequestDiagnostics {
        request_lowering_mode: request_lowering_mode.into(),
        anthropic_cache: None,
        anthropic_context_management: None,
        openai_request_controls,
        openai_remote_compaction: None,
        incremental_continuation: Some(ProviderIncrementalContinuationDiagnostics {
            status: "fallback_full_request".into(),
            fallback_reason: Some(fallback_reason.into()),
            incremental_input_items,
            full_input_items: Some(full_input_items),
            expected_prefix_items: Some(mismatch.expected_prefix_items),
            first_mismatch_index: mismatch.first_mismatch_index,
            previous_item_type: mismatch.previous_item_type,
            current_item_type: mismatch.current_item_type,
            previous_item_id: mismatch.previous_item_id,
            current_item_id: mismatch.current_item_id,
            previous_item_hash: mismatch.previous_item_hash,
            current_item_hash: mismatch.current_item_hash,
            request_shape_hash: mismatch.request_shape_hash,
            first_mismatch_path: mismatch.first_mismatch_path,
            mismatch_kind: mismatch.mismatch_kind,
        }),
        native_web_search,
        response_format,
    }
}

fn response_format_diagnostics(
    lowered: bool,
    request: &ProviderTurnRequest,
) -> Option<ProviderResponseFormatDiagnostics> {
    match request.response_format.as_ref()? {
        ProviderResponseFormatRequest::JsonSchema(format) => {
            Some(ProviderResponseFormatDiagnostics {
                requested: true,
                lowered,
                format_type: "json_schema".into(),
                schema_name: Some(format.name.clone()),
                fallback_reason: (!lowered)
                    .then(|| "transport does not support JSON Schema response format".into()),
            })
        }
    }
}

fn native_web_search_diagnostics(
    request: &ProviderTurnRequest,
) -> Option<ProviderNativeWebSearchDiagnostics> {
    let native = request.native_web_search.as_ref()?;
    let lowered = matches!(
        native.kind,
        ProviderNativeWebSearchKind::OpenAi | ProviderNativeWebSearchKind::Xai
    );
    Some(ProviderNativeWebSearchDiagnostics {
        kind: native.kind,
        provider_id: native.provider_id.clone(),
        provider_model_ref: native.provider_model_ref.clone(),
        advertised_tool_type: native.advertised_tool_type.clone(),
        backend_kind: native.backend_kind.clone(),
        lowered,
        fallback_reason: (!lowered).then(|| {
            "openai responses transport only supports OpenAI/xAI-native web search".into()
        }),
    })
}

fn update_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<OpenAiContinuationScope>,
    request_shape: OpenAiRequestShape,
    append_match_input: Vec<Value>,
    provider_input: Vec<Value>,
    parsed: &ParsedOpenAiResponse,
) {
    let Some(scope) = scope else {
        return;
    };
    let mut state = lock_openai_continuation(continuation);
    let latest_input_tokens = parsed.response.input_tokens;
    let next = match (parsed.response_id.as_ref(), parsed.output_items.is_empty()) {
        (Some(response_id), false) => {
            state.next_generation = state.next_generation.saturating_add(1);
            let mut items = provider_input
                .into_iter()
                .map(|item| canonicalize_openai_provider_item(&item))
                .collect::<Vec<_>>();
            items.extend(
                parsed
                    .output_items
                    .iter()
                    .map(canonicalize_openai_provider_item),
            );
            let mut append_match_items = append_match_input;
            append_match_items.extend(openai_append_match_output_items(&parsed.output_items));
            Some(OpenAiProviderWindow {
                response_id: Some(response_id.clone()),
                request_shape,
                latest_compaction_index: latest_openai_compaction_index(&items),
                items,
                append_match_items,
                generation: state.next_generation,
                latest_input_tokens,
            })
        }
        _ => None,
    };
    if let Some(next) = next {
        state.windows.insert(scope, next);
    } else {
        state.windows.remove(&scope);
    }
}

fn openai_append_match_output_items(output_items: &[Value]) -> Vec<Value> {
    output_items
        .iter()
        .filter_map(openai_append_match_output_item)
        .collect()
}

fn openai_append_match_output_item(item: &Value) -> Option<Value> {
    if matches!(item.get("type").and_then(Value::as_str), Some("reasoning")) {
        None
    } else {
        Some(canonicalize_openai_append_match_item(item))
    }
}

async fn maybe_compact_openai_provider_window(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<&OpenAiContinuationScope>,
    request_shape: &OpenAiRequestShape,
    compaction_policy: OpenAiCompactionPolicy,
    client: &Client,
    compact_url: String,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Option<ProviderOpenAiRemoteCompactionDiagnostics> {
    let Some(scope) = scope else {
        return None;
    };
    let window = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).cloned()
    }?;
    let Some(trigger) =
        openai_compaction_trigger_for_window(&window, request_shape, compaction_policy)
    else {
        return None;
    };
    let candidate = match openai_provider_window_compaction_candidate(&window) {
        Ok(candidate) => candidate,
        Err(skip_reason) => {
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: format!("skipped_{skip_reason}"),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(window.items.len()),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: window.latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash(request_shape)),
                continuation_generation: Some(window.generation),
                error: None,
            });
        }
    };

    let input_items = candidate.items.len();
    let request_shape_hash = request_shape_hash(request_shape);
    if let Some(http_status) = compact_endpoint_unsupported_status(continuation, &compact_url) {
        return Some(openai_compact_unsupported_diagnostics(
            "skipped_unsupported_endpoint",
            trigger.reason,
            input_items,
            candidate.latest_compaction_index,
            trigger.estimated_input_tokens,
            Some(trigger.trigger_input_tokens),
            Some(request_shape_hash),
            Some(window.generation),
            http_status,
            None,
        ));
    }
    let compact_body = build_openai_compact_request_body(request_shape, &candidate.items);
    let compacted = match send_openai_compact_request(
        client,
        compact_url.clone(),
        compact_body,
        headers,
        trace,
        agent_id,
    )
    .await
    {
        Ok(compacted) => compacted,
        Err(error) => {
            if is_non_persisted_compact_item_id_error(&error) {
                return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                    status: "invalid_non_persisted_item_id".into(),
                    trigger_reason: Some(trigger.reason.into()),
                    endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                    http_status: error_status(&error),
                    input_items: Some(input_items),
                    output_items: None,
                    compaction_items: None,
                    latest_compaction_index: candidate.latest_compaction_index,
                    estimated_input_tokens: trigger.estimated_input_tokens,
                    trigger_input_tokens: Some(trigger.trigger_input_tokens),
                    encrypted_content_hashes: None,
                    encrypted_content_bytes: None,
                    request_shape_hash: Some(request_shape_hash),
                    continuation_generation: Some(window.generation),
                    error: Some(error.to_string()),
                });
            }
            if let Some(http_status) = unsupported_compact_endpoint_status(&error) {
                mark_compact_endpoint_unsupported(continuation, &compact_url, http_status);
                return Some(openai_compact_unsupported_diagnostics(
                    "unsupported_endpoint",
                    trigger.reason,
                    input_items,
                    candidate.latest_compaction_index,
                    trigger.estimated_input_tokens,
                    Some(trigger.trigger_input_tokens),
                    Some(request_shape_hash),
                    Some(window.generation),
                    http_status,
                    Some(error.to_string()),
                ));
            }
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: "failed".into(),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: error_status(&error),
                input_items: Some(input_items),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: candidate.latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash),
                continuation_generation: Some(window.generation),
                error: Some(error.to_string()),
            });
        }
    };
    if compacted.is_empty() {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_empty_output".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(0),
            compaction_items: Some(0),
            latest_compaction_index: candidate.latest_compaction_index,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(window.generation),
            error: Some("OpenAI compact response returned an empty output window".into()),
        });
    }

    let latest_compaction_index = latest_openai_compaction_index(&compacted);
    let encrypted_content_hashes = openai_compaction_encrypted_content_hashes(&compacted);
    let encrypted_content_bytes = openai_compaction_encrypted_content_bytes(&compacted);
    let compaction_items = encrypted_content_hashes.len();
    if compaction_items == 0 {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_missing_compaction_item".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(0),
            latest_compaction_index: None,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(window.generation),
            error: Some("OpenAI compact response did not include a compaction item".into()),
        });
    }

    let output_items = compacted.len();
    let generation = {
        let mut state = lock_openai_continuation(continuation);
        let current_generation = state.windows.get(scope).map(|current| current.generation);
        if current_generation != Some(window.generation) {
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: "stale_generation".into(),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(input_items),
                output_items: Some(output_items),
                compaction_items: Some(compaction_items),
                latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: Some(encrypted_content_hashes),
                encrypted_content_bytes: Some(encrypted_content_bytes),
                request_shape_hash: Some(request_shape_hash),
                continuation_generation: current_generation,
                error: Some(format!(
                    "OpenAI provider window advanced while compact request was in flight; captured generation {}",
                    window.generation
                )),
            });
        }
        state.next_generation = state.next_generation.saturating_add(1);
        let generation = state.next_generation;
        let mut items = openai_compacted_replay_items(&compacted);
        items.extend(candidate.retained_tail.clone());
        let latest_compaction_index = latest_openai_compaction_index(&items);
        state.windows.insert(
            scope.clone(),
            OpenAiProviderWindow {
                response_id: None,
                request_shape: request_shape.clone(),
                items,
                append_match_items: window.append_match_items,
                latest_compaction_index,
                latest_input_tokens: 0,
                generation,
            },
        );
        generation
    };

    Some(ProviderOpenAiRemoteCompactionDiagnostics {
        status: "compacted".into(),
        trigger_reason: Some(trigger.reason.into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: None,
        input_items: Some(input_items),
        output_items: Some(output_items),
        compaction_items: Some(compaction_items),
        latest_compaction_index,
        estimated_input_tokens: trigger.estimated_input_tokens,
        trigger_input_tokens: Some(trigger.trigger_input_tokens),
        encrypted_content_hashes: Some(encrypted_content_hashes),
        encrypted_content_bytes: Some(encrypted_content_bytes),
        request_shape_hash: Some(request_shape_hash),
        continuation_generation: Some(generation),
        error: None,
    })
}

async fn maybe_compact_openai_request_plan(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    plan: &mut OpenAiRequestPlan,
    compaction_policy: OpenAiCompactionPolicy,
    client: &Client,
    compact_url: String,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Option<ProviderOpenAiRemoteCompactionDiagnostics> {
    if plan.diagnostics.request_lowering_mode != "incremental_continuation" {
        return None;
    }
    let scope = plan.scope.as_ref()?;
    let previous = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).cloned()
    }?;
    previous.response_id.as_ref()?;

    let mut compactable_items = previous.items.clone();
    compactable_items.extend(plan.provider_input.clone());
    let compactable_window = OpenAiProviderWindow {
        response_id: None,
        request_shape: plan.request_shape.clone(),
        latest_compaction_index: latest_openai_compaction_index(&compactable_items),
        items: compactable_items,
        append_match_items: plan.append_match_input.clone(),
        latest_input_tokens: previous.latest_input_tokens,
        generation: previous.generation,
    };
    let Some(trigger) =
        openai_compaction_trigger_for_request_plan(&previous, plan, compaction_policy)
    else {
        return None;
    };
    let candidate = match openai_provider_window_compaction_candidate(&compactable_window) {
        Ok(candidate) => candidate,
        Err(skip_reason) => {
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: format!("skipped_{skip_reason}"),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: None,
                input_items: Some(compactable_window.items.len()),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: compactable_window.latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash(&plan.request_shape)),
                continuation_generation: Some(previous.generation),
                error: None,
            });
        }
    };

    let input_items = candidate.items.len();
    let request_shape_hash = request_shape_hash(&plan.request_shape);
    if let Some(http_status) = compact_endpoint_unsupported_status(continuation, &compact_url) {
        return Some(openai_compact_unsupported_diagnostics(
            "skipped_unsupported_endpoint",
            trigger.reason,
            input_items,
            candidate.latest_compaction_index,
            trigger.estimated_input_tokens,
            Some(trigger.trigger_input_tokens),
            Some(request_shape_hash),
            Some(previous.generation),
            http_status,
            None,
        ));
    }
    let compact_body = build_openai_compact_request_body(&plan.request_shape, &candidate.items);
    let compacted = match send_openai_compact_request(
        client,
        compact_url.clone(),
        compact_body,
        headers,
        trace,
        agent_id,
    )
    .await
    {
        Ok(compacted) => compacted,
        Err(error) => {
            if is_non_persisted_compact_item_id_error(&error) {
                return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                    status: "invalid_non_persisted_item_id".into(),
                    trigger_reason: Some(trigger.reason.into()),
                    endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                    http_status: error_status(&error),
                    input_items: Some(input_items),
                    output_items: None,
                    compaction_items: None,
                    latest_compaction_index: candidate.latest_compaction_index,
                    estimated_input_tokens: trigger.estimated_input_tokens,
                    trigger_input_tokens: Some(trigger.trigger_input_tokens),
                    encrypted_content_hashes: None,
                    encrypted_content_bytes: None,
                    request_shape_hash: Some(request_shape_hash),
                    continuation_generation: Some(previous.generation),
                    error: Some(error.to_string()),
                });
            }
            if let Some(http_status) = unsupported_compact_endpoint_status(&error) {
                mark_compact_endpoint_unsupported(continuation, &compact_url, http_status);
                return Some(openai_compact_unsupported_diagnostics(
                    "unsupported_endpoint",
                    trigger.reason,
                    input_items,
                    candidate.latest_compaction_index,
                    trigger.estimated_input_tokens,
                    Some(trigger.trigger_input_tokens),
                    Some(request_shape_hash),
                    Some(previous.generation),
                    http_status,
                    Some(error.to_string()),
                ));
            }
            return Some(ProviderOpenAiRemoteCompactionDiagnostics {
                status: "failed".into(),
                trigger_reason: Some(trigger.reason.into()),
                endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
                http_status: error_status(&error),
                input_items: Some(input_items),
                output_items: None,
                compaction_items: None,
                latest_compaction_index: candidate.latest_compaction_index,
                estimated_input_tokens: trigger.estimated_input_tokens,
                trigger_input_tokens: Some(trigger.trigger_input_tokens),
                encrypted_content_hashes: None,
                encrypted_content_bytes: None,
                request_shape_hash: Some(request_shape_hash),
                continuation_generation: Some(previous.generation),
                error: Some(error.to_string()),
            });
        }
    };
    if compacted.is_empty() {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_empty_output".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(0),
            compaction_items: Some(0),
            latest_compaction_index: candidate.latest_compaction_index,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(previous.generation),
            error: Some("OpenAI compact response returned an empty output window".into()),
        });
    }

    let latest_compaction_index = latest_openai_compaction_index(&compacted);
    let encrypted_content_hashes = openai_compaction_encrypted_content_hashes(&compacted);
    let encrypted_content_bytes = openai_compaction_encrypted_content_bytes(&compacted);
    let compaction_items = encrypted_content_hashes.len();
    if compaction_items == 0 {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "rejected_missing_compaction_item".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(0),
            latest_compaction_index: None,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(Vec::new()),
            encrypted_content_bytes: Some(Vec::new()),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: Some(previous.generation),
            error: Some("OpenAI compact response did not include a compaction item".into()),
        });
    }

    let current_generation = {
        let state = lock_openai_continuation(continuation);
        state.windows.get(scope).map(|current| current.generation)
    };
    if current_generation != Some(previous.generation) {
        return Some(ProviderOpenAiRemoteCompactionDiagnostics {
            status: "stale_generation".into(),
            trigger_reason: Some(trigger.reason.into()),
            endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
            http_status: None,
            input_items: Some(input_items),
            output_items: Some(compacted.len()),
            compaction_items: Some(compaction_items),
            latest_compaction_index,
            estimated_input_tokens: trigger.estimated_input_tokens,
            trigger_input_tokens: Some(trigger.trigger_input_tokens),
            encrypted_content_hashes: Some(encrypted_content_hashes),
            encrypted_content_bytes: Some(encrypted_content_bytes),
            request_shape_hash: Some(request_shape_hash),
            continuation_generation: current_generation,
            error: Some(format!(
                "OpenAI provider window advanced while compact request was in flight; captured generation {}",
                previous.generation
            )),
        });
    }

    let output_items = compacted.len();
    let mut provider_input = openai_compacted_replay_items(&compacted);
    provider_input.extend(candidate.retained_tail.clone());
    plan.body["input"] = Value::Array(provider_input.clone());
    if let Some(object) = plan.body.as_object_mut() {
        object.remove("previous_response_id");
    }
    plan.provider_input = provider_input;
    plan.diagnostics.request_lowering_mode = "provider_window_compacted".into();

    Some(ProviderOpenAiRemoteCompactionDiagnostics {
        status: "compacted".into(),
        trigger_reason: Some(trigger.reason.into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: None,
        input_items: Some(input_items),
        output_items: Some(output_items),
        compaction_items: Some(compaction_items),
        latest_compaction_index,
        estimated_input_tokens: trigger.estimated_input_tokens,
        trigger_input_tokens: Some(trigger.trigger_input_tokens),
        encrypted_content_hashes: Some(encrypted_content_hashes),
        encrypted_content_bytes: Some(encrypted_content_bytes),
        request_shape_hash: Some(request_shape_hash),
        continuation_generation: Some(previous.generation),
        error: None,
    })
}

fn compact_endpoint_unsupported_status(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    compact_url: &str,
) -> Option<u16> {
    let state = lock_openai_continuation(continuation);
    state
        .unsupported_compact_endpoints
        .get(compact_url)
        .copied()
}

fn mark_compact_endpoint_unsupported(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    compact_url: &str,
    http_status: u16,
) {
    let mut state = lock_openai_continuation(continuation);
    state
        .unsupported_compact_endpoints
        .insert(compact_url.to_string(), http_status);
}

fn unsupported_compact_endpoint_status(error: &anyhow::Error) -> Option<u16> {
    if is_non_persisted_compact_item_id_error(error) {
        return None;
    }
    let status = error_status(error)?;
    match status {
        404 | 405 | 410 | 501 => Some(status),
        _ => None,
    }
}

fn is_non_persisted_compact_item_id_error(error: &anyhow::Error) -> bool {
    error_status(error) == Some(404)
        && error
            .to_string()
            .contains("Items are not persisted when `store` is set to false")
}

fn error_status(error: &anyhow::Error) -> Option<u16> {
    error
        .downcast_ref::<ProviderTransportError>()
        .and_then(|error| error.status)
}

fn openai_compact_unsupported_diagnostics(
    status: &str,
    trigger_reason: &str,
    input_items: usize,
    latest_compaction_index: Option<usize>,
    estimated_input_tokens: Option<u64>,
    trigger_input_tokens: Option<u64>,
    request_shape_hash: Option<String>,
    continuation_generation: Option<u64>,
    http_status: u16,
    error: Option<String>,
) -> ProviderOpenAiRemoteCompactionDiagnostics {
    ProviderOpenAiRemoteCompactionDiagnostics {
        status: status.into(),
        trigger_reason: Some(trigger_reason.into()),
        endpoint_kind: Some(OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND.into()),
        http_status: Some(http_status),
        input_items: Some(input_items),
        output_items: None,
        compaction_items: None,
        latest_compaction_index,
        estimated_input_tokens,
        trigger_input_tokens,
        encrypted_content_hashes: None,
        encrypted_content_bytes: None,
        request_shape_hash,
        continuation_generation,
        error,
    }
}

#[derive(Clone, Copy, Debug)]
struct OpenAiCompactionTrigger {
    reason: &'static str,
    estimated_input_tokens: Option<u64>,
    trigger_input_tokens: u64,
}

fn openai_compaction_trigger_for_window(
    window: &OpenAiProviderWindow,
    request_shape: &OpenAiRequestShape,
    policy: OpenAiCompactionPolicy,
) -> Option<OpenAiCompactionTrigger> {
    if window.latest_input_tokens > 0 {
        return (window.latest_input_tokens >= policy.trigger_input_tokens).then_some(
            OpenAiCompactionTrigger {
                reason: "token_budget_pressure",
                estimated_input_tokens: None,
                trigger_input_tokens: policy.trigger_input_tokens,
            },
        );
    }

    let estimated = estimate_openai_provider_payload_tokens(
        request_shape,
        openai_items_after_latest_compaction(&window.items),
    );
    (estimated >= policy.trigger_input_tokens).then_some(OpenAiCompactionTrigger {
        reason: "estimated_window_pressure",
        estimated_input_tokens: Some(estimated),
        trigger_input_tokens: policy.trigger_input_tokens,
    })
}

fn openai_compaction_trigger_for_request_plan(
    previous: &OpenAiProviderWindow,
    plan: &OpenAiRequestPlan,
    policy: OpenAiCompactionPolicy,
) -> Option<OpenAiCompactionTrigger> {
    let mut compactable_items = previous.items.clone();
    compactable_items.extend(plan.provider_input.clone());
    if previous.latest_input_tokens == 0
        && latest_openai_compaction_index(&compactable_items).is_some()
    {
        return None;
    }
    let estimated = estimate_openai_provider_payload_tokens(
        &plan.request_shape,
        openai_items_after_latest_compaction(&compactable_items),
    );
    (estimated >= policy.trigger_input_tokens).then_some(OpenAiCompactionTrigger {
        reason: "estimated_window_pressure",
        estimated_input_tokens: Some(estimated),
        trigger_input_tokens: policy.trigger_input_tokens,
    })
}

fn estimate_openai_provider_payload_tokens(
    request_shape: &OpenAiRequestShape,
    input_items: &[Value],
) -> u64 {
    let shape_tokens = estimate_json_tokens(&request_shape.wire_shape);
    let input_tokens = input_items
        .iter()
        .map(estimate_json_tokens)
        .fold(0usize, usize::saturating_add);
    shape_tokens.saturating_add(input_tokens).saturating_add(1) as u64
}

fn openai_items_after_latest_compaction(items: &[Value]) -> &[Value] {
    latest_openai_compaction_index(items)
        .map(|index| &items[index.saturating_add(1)..])
        .unwrap_or(items)
}

fn openai_provider_window_compaction_candidate(
    window: &OpenAiProviderWindow,
) -> std::result::Result<OpenAiCompactionCandidate, &'static str> {
    let boundary =
        latest_complete_openai_tool_call_boundary(&window.items).ok_or("no_safe_boundary")?;
    debug_assert!(boundary > 0);

    let compact_items = window.items[..boundary].to_vec();
    if has_unpaired_openai_tool_call(&compact_items) {
        return Err("unpaired_tool_call");
    }

    Ok(OpenAiCompactionCandidate {
        latest_compaction_index: latest_openai_compaction_index(&compact_items),
        items: compact_items,
        retained_tail: window.items[boundary..].to_vec(),
    })
}

fn openai_compacted_replay_items(compacted: &[Value]) -> Vec<Value> {
    latest_openai_compaction_index(compacted)
        .map(|index| compacted[index..].to_vec())
        .unwrap_or_else(|| compacted.to_vec())
}

fn latest_complete_openai_tool_call_boundary(items: &[Value]) -> Option<usize> {
    let mut function_calls = HashSet::new();
    let mut custom_tool_calls = HashSet::new();
    let mut function_outputs = HashSet::new();
    let mut custom_tool_outputs = HashSet::new();
    let mut latest_complete_boundary = None;

    for (index, item) in items.iter().enumerate() {
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            if openai_tool_call_sets_are_complete(
                &function_calls,
                &function_outputs,
                &custom_tool_calls,
                &custom_tool_outputs,
            ) {
                latest_complete_boundary = Some(index + 1);
            }
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                function_calls.insert(call_id.to_string());
            }
            Some("custom_tool_call") => {
                custom_tool_calls.insert(call_id.to_string());
            }
            Some("function_call_output") => {
                function_outputs.insert(call_id.to_string());
            }
            Some("custom_tool_call_output") => {
                custom_tool_outputs.insert(call_id.to_string());
            }
            _ => {}
        }
        if openai_tool_call_sets_are_complete(
            &function_calls,
            &function_outputs,
            &custom_tool_calls,
            &custom_tool_outputs,
        ) {
            latest_complete_boundary = Some(index + 1);
        }
    }

    latest_complete_boundary
}

fn openai_tool_call_sets_are_complete(
    function_calls: &HashSet<String>,
    function_outputs: &HashSet<String>,
    custom_tool_calls: &HashSet<String>,
    custom_tool_outputs: &HashSet<String>,
) -> bool {
    function_calls.is_subset(function_outputs) && custom_tool_calls.is_subset(custom_tool_outputs)
}

fn has_unpaired_openai_tool_call(items: &[Value]) -> bool {
    let mut function_calls = HashSet::new();
    let mut custom_tool_calls = HashSet::new();
    let mut function_outputs = HashSet::new();
    let mut custom_tool_outputs = HashSet::new();

    for item in items {
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                function_calls.insert(call_id.to_string());
            }
            Some("custom_tool_call") => {
                custom_tool_calls.insert(call_id.to_string());
            }
            Some("function_call_output") => {
                function_outputs.insert(call_id.to_string());
            }
            Some("custom_tool_call_output") => {
                custom_tool_outputs.insert(call_id.to_string());
            }
            _ => {}
        }
    }

    !openai_tool_call_sets_are_complete(
        &function_calls,
        &function_outputs,
        &custom_tool_calls,
        &custom_tool_outputs,
    )
}

fn build_openai_compact_request_body(request_shape: &OpenAiRequestShape, items: &[Value]) -> Value {
    let compact_items = sanitize_openai_store_false_compact_items(items);
    let mut body = json!({
        "model": request_shape.wire_shape.get("model").cloned().unwrap_or(Value::Null),
        "input": compact_items,
        "instructions": request_shape
            .wire_shape
            .get("instructions")
            .cloned()
            .unwrap_or_else(|| Value::String(request_shape.prompt_frame.system_prompt.clone())),
        "tools": request_shape
            .wire_shape
            .get("tools")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        "parallel_tool_calls": request_shape
            .wire_shape
            .get("parallel_tool_calls")
            .cloned()
            .unwrap_or(Value::Bool(false)),
    });
    if let Some(reasoning) = request_shape.wire_shape.get("reasoning") {
        if !reasoning.is_null() {
            body["reasoning"] = reasoning.clone();
        }
    }
    if let Some(text) = request_shape.wire_shape.get("text") {
        body["text"] = text.clone();
    }
    body
}

fn sanitize_openai_store_false_compact_items(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .map(canonicalize_openai_provider_item)
        .collect()
}

fn openai_append_match_input_items(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .map(canonicalize_openai_append_match_item)
        .collect()
}

// Provider windows are replayed into future OpenAI requests and compact calls.
// Keep their wire shape close to OpenAI's item contract while stripping fields
// that are not accepted on replay.
fn canonicalize_openai_provider_item(item: &Value) -> Value {
    let mut item = openai_without_provider_item_id(item);
    let Some(object) = item.as_object_mut() else {
        return item;
    };
    if object.get("type").and_then(Value::as_str) == Some("compaction_summary") {
        object.insert("type".into(), Value::String("compaction".into()));
    }
    if matches!(
        object.get("type").and_then(Value::as_str),
        Some("message" | "function_call" | "custom_tool_call" | "reasoning" | "compaction")
    ) {
        object.remove("status");
    }
    if object.get("type").and_then(Value::as_str) == Some("function_call") {
        if let Some(arguments) = object.get("arguments").and_then(Value::as_str) {
            if let Ok(parsed_arguments) = serde_json::from_str::<Value>(arguments) {
                object.insert(
                    "arguments".into(),
                    Value::String(canonical_json(&parsed_arguments)),
                );
            }
        }
    }
    item
}

// Append matching compares provider outputs against Holon-rebuilt input. Use a
// semantic form that preserves item order and conversational meaning while
// ignoring provider-only metadata and nested text decorations.
fn canonicalize_openai_append_match_item(item: &Value) -> Value {
    let item = openai_without_provider_item_id(item);
    let Some(object) = item.as_object() else {
        return item;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("message") => canonicalize_openai_append_match_message(object),
        Some("function_call") => canonicalize_openai_append_match_function_call(object),
        Some("custom_tool_call") => canonicalize_openai_append_match_custom_tool_call(object),
        Some("function_call_output" | "custom_tool_call_output") => json!({
            "type": object.get("type").cloned().unwrap_or(Value::Null),
            "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
            "output": object.get("output").cloned().unwrap_or(Value::Null),
        }),
        Some("compaction_summary") => {
            let mut canonical = json!({ "type": "compaction" });
            if let Some(encrypted_content) = object.get("encrypted_content") {
                canonical["encrypted_content"] = encrypted_content.clone();
            }
            canonical
        }
        Some("compaction") => {
            let mut canonical = json!({ "type": "compaction" });
            if let Some(encrypted_content) = object.get("encrypted_content") {
                canonical["encrypted_content"] = encrypted_content.clone();
            }
            canonical
        }
        Some(_) | None => canonicalize_openai_provider_item(&item),
    }
}

fn canonicalize_openai_append_match_message(object: &serde_json::Map<String, Value>) -> Value {
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| canonicalize_openai_append_match_content_item(role, item))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

fn canonicalize_openai_append_match_content_item(role: &str, item: &Value) -> Value {
    let Some(object) = item.as_object() else {
        return item.clone();
    };
    let item_type = object.get("type").and_then(Value::as_str);
    if matches!(
        item_type,
        Some("output_text" | "input_text" | "text" | "message_text")
    ) {
        let normalized_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        return json!({
            "type": normalized_type,
            "text": object.get("text").cloned().unwrap_or(Value::String(String::new())),
        });
    }
    let mut canonical = serde_json::Map::new();
    if let Some(item_type) = object.get("type") {
        canonical.insert("type".into(), item_type.clone());
    }
    for key in ["text", "image_url", "file_id", "filename"] {
        if let Some(value) = object.get(key) {
            canonical.insert(key.into(), value.clone());
        }
    }
    Value::Object(canonical)
}

fn canonicalize_openai_append_match_function_call(
    object: &serde_json::Map<String, Value>,
) -> Value {
    let arguments = object
        .get("arguments")
        .and_then(Value::as_str)
        .map(canonicalize_openai_arguments_string)
        .map(Value::String)
        .unwrap_or_else(|| object.get("arguments").cloned().unwrap_or(Value::Null));
    json!({
        "type": "function_call",
        "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
        "name": object.get("name").cloned().unwrap_or(Value::Null),
        "arguments": arguments,
    })
}

fn canonicalize_openai_append_match_custom_tool_call(
    object: &serde_json::Map<String, Value>,
) -> Value {
    json!({
        "type": "custom_tool_call",
        "call_id": object.get("call_id").cloned().unwrap_or(Value::Null),
        "name": object.get("name").cloned().unwrap_or(Value::Null),
        "input": object.get("input").cloned().unwrap_or(Value::Null),
    })
}

fn canonicalize_openai_arguments_string(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .map(|parsed| canonical_json(&parsed))
        .unwrap_or_else(|_| arguments.to_string())
}

fn openai_without_provider_item_id(item: &Value) -> Value {
    let mut item = item.clone();
    let Some(object) = item.as_object_mut() else {
        return item;
    };
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Some(id) = id.as_deref() {
        match object.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call") if !object.contains_key("call_id") => {
                object.insert("call_id".into(), Value::String(id.to_string()));
            }
            _ => {}
        }
    }
    object.remove("id");
    item
}

fn openai_compaction_encrypted_content_hashes(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter(|item| openai_is_compaction_item(item))
        .filter_map(|item| item.get("encrypted_content").and_then(Value::as_str))
        .map(|content| sha256_hex(content.as_bytes()))
        .collect()
}

fn openai_compaction_encrypted_content_bytes(items: &[Value]) -> Vec<usize> {
    items
        .iter()
        .filter(|item| openai_is_compaction_item(item))
        .filter_map(|item| item.get("encrypted_content").and_then(Value::as_str))
        .map(str::len)
        .collect()
}

fn invalidate_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
    scope: Option<&OpenAiContinuationScope>,
) {
    let mut state = lock_openai_continuation(continuation);
    if let Some(scope) = scope {
        state.windows.remove(scope);
    } else {
        state.windows.clear();
    }
}

fn lock_openai_continuation(
    continuation: &Arc<Mutex<OpenAiContinuationState>>,
) -> MutexGuard<'_, OpenAiContinuationState> {
    match continuation.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(crate) fn build_openai_input(conversation: &[ConversationMessage]) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    let mut custom_tool_calls = HashMap::<String, bool>::new();
    for message in conversation {
        match message {
            ConversationMessage::UserText(text) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }],
            })),
            ConversationMessage::UserBlocks(blocks) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": blocks.iter().map(|block| json!({
                    "type": "input_text",
                    "text": block.text,
                })).collect::<Vec<_>>(),
            })),
            ConversationMessage::UserImage {
                prompt,
                media_type,
                data_base64,
            } => items.push(json!({
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": prompt },
                    {
                        "type": "input_image",
                        "image_url": format!("data:{media_type};base64,{data_base64}"),
                    },
                ],
            })),
            ConversationMessage::AssistantBlocks(blocks) => {
                let mut pending_text = Vec::new();
                for block in blocks {
                    match block {
                        ModelBlock::Text { text } => pending_text.push(text.clone()),
                        ModelBlock::ToolUse { id, name, input } => {
                            flush_assistant_text(&mut items, &mut pending_text);
                            if let Some(raw_input) = openai_custom_tool_input(name, input) {
                                custom_tool_calls.insert(id.clone(), true);
                                items.push(json!({
                                    "type": "custom_tool_call",
                                    "call_id": id,
                                    "name": name,
                                    "input": raw_input,
                                }));
                            } else {
                                custom_tool_calls.insert(id.clone(), false);
                                items.push(json!({
                                    "type": "function_call",
                                    "call_id": id,
                                    "name": name,
                                    "arguments": canonical_json(input),
                                }));
                            }
                        }
                        ModelBlock::Thinking { .. } | ModelBlock::RedactedThinking { .. } => {}
                    }
                }
                flush_assistant_text(&mut items, &mut pending_text);
            }
            ConversationMessage::UserToolResults(results) => {
                for result in results {
                    let item_type = if custom_tool_calls
                        .get(&result.tool_use_id)
                        .copied()
                        .unwrap_or(false)
                    {
                        "custom_tool_call_output"
                    } else {
                        "function_call_output"
                    };
                    items.push(json!({
                        "type": item_type,
                        "call_id": result.tool_use_id,
                        "output": result.content,
                    }));
                }
            }
        }
    }
    Ok(items)
}

fn openai_custom_tool_input(name: &str, input: &Value) -> Option<String> {
    if name != crate::tool::tools::apply_patch_tool::NAME {
        return None;
    }
    match input {
        Value::String(value) => Some(value.clone()),
        Value::Object(map) => map
            .get("patch")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        _ => None,
    }
}

fn flush_assistant_text(items: &mut Vec<Value>, pending_text: &mut Vec<String>) {
    if pending_text.is_empty() {
        return;
    }
    let content = pending_text
        .drain(..)
        .map(|text| json!({ "type": "output_text", "text": text }))
        .collect::<Vec<_>>();
    items.push(json!({
        "type": "message",
        "role": "assistant",
        "content": content,
    }));
}

async fn send_chat_completion_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<ParsedOpenAiResponse> {
    let model_ref = provider_model_ref("openai", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai",
            Some(&model_ref),
            url.as_str(),
            "chat_completions",
            &headers,
            &body,
        )
    });
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = send_openai_request(
        request.json(&body),
        "OpenAI Chat Completions request failed",
        "request_send",
        "openai",
        Some(&model_ref),
        Some(url.as_str()),
        true,
        request_trace.as_ref(),
    )
    .await?;
    trace_response_headers(
        request_trace.as_ref(),
        response.status(),
        response.headers(),
    );
    let provider_request_id = provider_request_id_from_headers(response.headers());

    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        trace_response_body(request_trace.as_ref(), &body);
        return Err(classify_chat_completion_status_error(
            "OpenAI Chat Completions request failed",
            status,
            body,
            Some(&model_ref),
            Some(url.as_str()),
            request_trace.as_ref(),
        ));
    }

    let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            return Err(classify_reqwest_transport_error_with_trace(
                "OpenAI Chat Completions response body failed",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                error,
                request_trace.as_ref(),
            ));
        }
        Err(_elapsed) => {
            return Err(timeout_transport_error_with_trace(
                "OpenAI Chat Completions response body read timed out",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                format!("timed out after {:?}", response_body_timeout()),
                request_trace.as_ref(),
            ));
        }
    };
    trace_response_body(request_trace.as_ref(), &body);

    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| invalid_response_error("invalid OpenAI Chat Completions JSON", error))?;

    parse_chat_completion_response(parsed)
        .map(|parsed| parsed.with_provider_request_id(provider_request_id))
}

fn classify_chat_completion_status_error(
    context: &str,
    status: reqwest::StatusCode,
    body: String,
    model_ref: Option<&str>,
    url: Option<&str>,
    trace: Option<&ProviderHttpTraceRequest>,
) -> anyhow::Error {
    // Try to parse as OpenAI error response
    if let Ok(error_json) = serde_json::from_str::<Value>(&body) {
        if let Some(error_obj) = error_json.get("error") {
            return classify_openai_chat_completion_error(
                context, error_obj, status, model_ref, url, trace,
            );
        }
    }

    // Fallback to generic status error classification
    classify_status_error_with_trace(
        context,
        "response_status",
        Some("openai"),
        model_ref,
        url,
        status,
        body,
        trace,
    )
}

pub(crate) fn classify_openai_chat_completion_error(
    context: &str,
    error: &Value,
    status: reqwest::StatusCode,
    model_ref: Option<&str>,
    url: Option<&str>,
    trace: Option<&ProviderHttpTraceRequest>,
) -> anyhow::Error {
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let error_message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    let error_code = error.get("code").and_then(Value::as_str);

    let classification = match error_code {
        Some("rate_limit_exceeded") | Some("rate_limit_exceeded_error") => {
            ProviderFailureClassification {
                kind: ProviderFailureKind::RateLimited,
                disposition: RetryDisposition::Retryable,
            }
        }
        Some("insufficient_quota") | Some("quota_exceeded") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("invalid_api_key") | Some("invalid_authentication") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("context_length_exceeded") | Some("max_tokens") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some("invalid_request_error") | Some("invalid_request") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some("server_error") | Some("service_unavailable") => ProviderFailureClassification {
            kind: ProviderFailureKind::ServerError,
            disposition: RetryDisposition::Retryable,
        },
        _ => match error_type {
            "rate_limit_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::RateLimited,
                disposition: RetryDisposition::Retryable,
            },
            "invalid_request_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::ContractError,
                disposition: RetryDisposition::FailFast,
            },
            "authentication_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::AuthError,
                disposition: RetryDisposition::FailFast,
            },
            "server_error" => ProviderFailureClassification {
                kind: ProviderFailureKind::ServerError,
                disposition: RetryDisposition::Retryable,
            },
            _ => ProviderFailureClassification {
                kind: ProviderFailureKind::ContractError,
                disposition: RetryDisposition::FailFast,
            },
        },
    };

    let detail = if let Some(code) = error_code {
        format!("{}: {}", code, error_message)
    } else {
        format!("{}: {}", error_type, error_message)
    };

    provider_transport_error(
        classification,
        Some(status.as_u16()),
        Some(crate::provider::ProviderTransportDiagnostics {
            stage: "response_status".into(),
            provider: Some("openai".into()),
            model_ref: model_ref.map(ToString::to_string),
            url: url.map(crate::provider::retry::sanitize_transport_url),
            status: Some(status.as_u16()),
            reqwest: None,
            http_trace: trace.and_then(|trace| trace.diagnostics(Some(status.as_u16()))),
            source_chain: Vec::new(),
        }),
        format!("{}: {}", context, detail),
    )
}

pub(crate) fn parse_chat_completion_response(response: Value) -> Result<ParsedOpenAiResponse> {
    // Extract response ID
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    // Extract choices array
    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI Chat Completions response did not contain choices array",
                "missing choices",
            )
        })?;

    let first_choice = choices.first().ok_or_else(|| {
        invalid_response_error(
            "OpenAI Chat Completions choices array was empty",
            "empty choices",
        )
    })?;

    // Extract message from first choice
    let message = first_choice.get("message").ok_or_else(|| {
        invalid_response_error(
            "OpenAI Chat Completions choice did not contain message",
            "missing message",
        )
    })?;

    // Parse message content
    let mut blocks = Vec::new();

    // Extract text content
    if let Some(content) = message.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            blocks.push(ModelBlock::Text {
                text: content.to_string(),
            });
        }
    }

    // Extract tool calls
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let id = tool_call.get("id").and_then(Value::as_str).ok_or_else(|| {
                invalid_response_error(
                    "OpenAI Chat Completions tool_call did not contain id",
                    "missing tool_call_id",
                )
            })?;

            let function = tool_call.get("function").ok_or_else(|| {
                invalid_response_error(
                    "OpenAI Chat Completions tool_call did not contain function",
                    "missing function",
                )
            })?;

            let name = function
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid_response_error(
                        "OpenAI Chat Completions function did not contain name",
                        "missing function_name",
                    )
                })?;

            let arguments_str = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");

            let arguments = if arguments_str.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(arguments_str).map_err(|error| {
                    invalid_response_error("invalid tool call arguments JSON", error)
                })?
            };

            blocks.push(ModelBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: arguments,
            });
        }
    }

    // Allow valid minimal assistant messages that contain neither text nor tool calls.
    // OpenAI Chat Completions can return empty/null content together with a finish_reason.
    // In such cases, we return an empty blocks vector rather than an error.
    if blocks.is_empty() {
        // Check if we have a valid finish_reason before accepting empty blocks
        let finish_reason = first_choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);

        if finish_reason.is_some() {
            // Accept empty response when we have a finish_reason
        } else {
            return Err(invalid_response_error(
                "OpenAI Chat Completions response contained no supported content",
                "empty content",
            ));
        }
    }

    // Extract usage
    let usage = response.get("usage").and_then(Value::as_object);
    let cache_usage = usage.map(|usage| ProviderCacheUsage {
        read_input_tokens: usage
            .get("prompt_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        creation_input_tokens: 0,
    });

    // Extract finish reason
    let stop_reason = first_choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);

    // Build output items for continuation tracking
    // Store the complete message object for proper continuation support
    let output_items = vec![message.clone()];

    Ok(ParsedOpenAiResponse {
        response: ProviderTurnResponse {
            blocks,
            stop_reason,
            input_tokens: usage
                .and_then(|usage| usage.get("prompt_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .and_then(|usage| usage.get("completion_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_usage,
            provider_message_id: response_id.clone(),
            provider_request_id: None,
            request_diagnostics: None,
        },
        response_id,
        output_items,
    })
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) async fn send_chat_completion_stream_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
) -> Result<ParsedOpenAiResponse> {
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = send_openai_request(
        request.json(&body),
        "OpenAI Chat Completions streaming request failed",
        "request_send",
        "openai",
        Some(&provider_model_ref("openai", &body)),
        Some(url.as_str()),
        true,
        None,
    )
    .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        return Err(classify_chat_completion_status_error(
            "OpenAI Chat Completions streaming request failed",
            status,
            body,
            None,
            None,
            None,
        ));
    }

    let response = read_chat_completion_stream(response).await?;
    parse_chat_completion_response(response)
}

#[cfg(test)]
#[allow(dead_code)]
async fn read_chat_completion_stream(response: Response) -> Result<Value> {
    const MAX_STREAMED_EVENTS: usize = 128;
    let mut streamed_events = Vec::new();

    let mut response = response;
    let mut pending = String::new();
    let mut data_lines = Vec::new();

    while let Some(chunk) = response.chunk().await.map_err(|error| {
        crate::provider::retry::classify_reqwest_transport_error_with_trace(
            "OpenAI Chat Completions streaming response failed",
            "streaming_response_body",
            "openai",
            None,
            None,
            error,
            None,
        )
    })? {
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline_idx) = pending.find('\n') {
            let mut line = pending[..newline_idx].to_string();
            pending.drain(..newline_idx + 1);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                if let Some(event) = process_chat_completion_sse_event(&mut data_lines)? {
                    match event {
                        ChatCompletionSseEvent::ContentDelta(delta) => {
                            if streamed_events.len() >= MAX_STREAMED_EVENTS {
                                return Err(invalid_response_error(
                                    "Chat Completions streaming exceeded maximum event count",
                                    "max_streamed_events_exceeded",
                                ));
                            }
                            streamed_events.push(json!({"delta": {"content": delta}}));
                        }
                        ChatCompletionSseEvent::ToolCallDelta(tool_call_delta) => {
                            if streamed_events.len() >= MAX_STREAMED_EVENTS {
                                return Err(invalid_response_error(
                                    "Chat Completions streaming exceeded maximum event count",
                                    "max_streamed_events_exceeded",
                                ));
                            }
                            // Extract the tool_calls array and store in delta format
                            if let Some(tool_calls_array) = tool_call_delta.get("tool_calls") {
                                streamed_events
                                    .push(json!({"delta": {"tool_calls": tool_calls_array}}));
                            }
                        }
                        ChatCompletionSseEvent::Done => {
                            // Stream ended
                            break;
                        }
                    }
                }
                continue;
            }
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start().to_string());
            }
        }
    }

    // Process remaining data
    if !pending.is_empty() {
        let line = pending.trim();
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }

    // Process final event
    if let Some(event) = process_chat_completion_sse_event(&mut data_lines)? {
        match event {
            ChatCompletionSseEvent::ContentDelta(delta) => {
                if streamed_events.len() >= MAX_STREAMED_EVENTS {
                    return Err(invalid_response_error(
                        "Chat Completions streaming exceeded maximum event count",
                        "max_streamed_events_exceeded",
                    ));
                }
                streamed_events.push(json!({"delta": {"content": delta}}));
            }
            ChatCompletionSseEvent::ToolCallDelta(tool_call_delta) => {
                if streamed_events.len() >= MAX_STREAMED_EVENTS {
                    return Err(invalid_response_error(
                        "Chat Completions streaming exceeded maximum event count",
                        "max_streamed_events_exceeded",
                    ));
                }
                // Extract the tool_calls array and store in delta format
                if let Some(tool_calls_array) = tool_call_delta.get("tool_calls") {
                    streamed_events.push(json!({"delta": {"tool_calls": tool_calls_array}}));
                }
            }
            ChatCompletionSseEvent::Done => {}
        }
    }

    // Accumulate streamed events into final response
    let accumulated = accumulate_chat_completion_stream_events(streamed_events)?;
    Ok(accumulated)
}

#[cfg(test)]
#[allow(dead_code)]
fn process_chat_completion_sse_event(
    data_lines: &mut Vec<String>,
) -> Result<Option<ChatCompletionSseEvent>> {
    if data_lines.is_empty() {
        return Ok(None);
    }

    let payload = data_lines.join("\n");
    data_lines.clear();
    let trimmed = payload.trim();

    if trimmed.is_empty() {
        return Ok(None);
    }

    if trimmed == "[DONE]" {
        return Ok(Some(ChatCompletionSseEvent::Done));
    }

    let event: Value = serde_json::from_str(trimmed).map_err(|error| {
        invalid_response_error("invalid Chat Completions streaming JSON", error)
    })?;

    // Check for errors
    if event.get("error").is_some() {
        return Err(invalid_response_error(
            "Chat Completions streaming contained error event",
            "error_in_stream",
        ));
    }

    // Process delta content from choices[0].delta (OpenAI Chat Completions streaming format)
    if let Some(choices) = event.get("choices").and_then(Value::as_array) {
        if let Some(first_choice) = choices.first() {
            if let Some(delta) = first_choice.get("delta") {
                // Process content delta
                if let Some(content) = delta.get("content") {
                    if let Some(text) = content.as_str() {
                        return Ok(Some(ChatCompletionSseEvent::ContentDelta(text.to_string())));
                    }
                }

                // Process tool_calls delta - return the entire array for accumulation
                if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    return Ok(Some(ChatCompletionSseEvent::ToolCallDelta(json!({
                        "tool_calls": tool_calls
                    }))));
                }
            }
        }
    }

    // Check for finish_reason at both top-level and choices[0] level
    if event.get("finish_reason").is_some() {
        // Stream ending event
        return Ok(Some(ChatCompletionSseEvent::Done));
    }

    // Also check for finish_reason in choices[0]
    if let Some(choices) = event.get("choices").and_then(Value::as_array) {
        if let Some(first_choice) = choices.first() {
            if first_choice.get("finish_reason").is_some() {
                // Stream ending event
                return Ok(Some(ChatCompletionSseEvent::Done));
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
pub(crate) fn accumulate_chat_completion_stream_events(events: Vec<Value>) -> Result<Value> {
    let mut content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish_reason = None;

    for event in events {
        // Handle both formats: direct delta and nested choices[0].delta
        let delta = if let Some(choices) = event.get("choices").and_then(Value::as_array) {
            choices.first().and_then(|c| c.get("delta"))
        } else {
            event.get("delta")
        };

        if let Some(text) = delta.and_then(|d| d.get("content")).and_then(Value::as_str) {
            content.push_str(text);
        }

        if let Some(tool_calls_delta) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for tool_call_delta in tool_calls_delta {
                let index = tool_call_delta["index"].as_u64().unwrap_or(0) as usize;
                while tool_calls.len() <= index {
                    tool_calls.push(json!({}));
                }

                let tool_call = &mut tool_calls[index];

                if let Some(id) = tool_call_delta.get("id").and_then(Value::as_str) {
                    tool_call["id"] = Value::String(id.to_string());
                }
                if let Some(name) = tool_call_delta
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    tool_call["function"]["name"] = Value::String(name.to_string());
                }
                if let Some(arguments) = tool_call_delta
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                {
                    let current_args = tool_call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .unwrap_or("");

                    let new_args = if let Some(additional) = arguments.as_str() {
                        // If we have existing content, concatenate; otherwise use new content
                        if current_args.is_empty() {
                            additional.to_string()
                        } else {
                            format!("{}{}", current_args, additional)
                        }
                    } else {
                        current_args.to_string()
                    };

                    // Only set if we have arguments
                    if !new_args.is_empty() {
                        tool_call["function"]["arguments"] = Value::String(new_args);
                    }
                }
            }
        }

        // Handle finish_reason in both formats: direct and nested in choices[0]
        if let Some(reason) = event.get("finish_reason").and_then(Value::as_str) {
            finish_reason = Some(reason.to_string());
        } else if let Some(choices) = event.get("choices").and_then(Value::as_array) {
            if let Some(first_choice) = choices.first() {
                if let Some(reason) = first_choice.get("finish_reason").and_then(Value::as_str) {
                    finish_reason = Some(reason.to_string());
                }
            }
        }
    }

    // Build accumulated response
    let mut message = json!({
        "role": "assistant",
        "content": content,
    });

    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    Ok(json!({
        "id": "chatcmpl-stream",
        "choices": [{
            "message": message,
            "finish_reason": finish_reason.unwrap_or("stop".to_string())
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    }))
}

#[cfg(test)]
#[allow(dead_code)]
enum ChatCompletionSseEvent {
    ContentDelta(String),
    ToolCallDelta(Value),
    Done,
}

async fn send_openai_responses_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<ParsedOpenAiResponse> {
    let model_ref = provider_model_ref("openai", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai",
            Some(&model_ref),
            url.as_str(),
            "responses",
            &headers,
            &body,
        )
    });
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = send_openai_request(
        request.json(&body),
        "OpenAI-style request failed",
        "request_send",
        "openai",
        Some(&model_ref),
        Some(url.as_str()),
        true,
        request_trace.as_ref(),
    )
    .await?;
    trace_response_headers(
        request_trace.as_ref(),
        response.status(),
        response.headers(),
    );
    let provider_request_id = provider_request_id_from_headers(response.headers());

    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        trace_response_body(request_trace.as_ref(), &body);
        return Err(classify_status_error_with_trace(
            "OpenAI-style request failed",
            "response_status",
            Some("openai"),
            Some(&model_ref),
            Some(url.as_str()),
            status,
            body,
            request_trace.as_ref(),
        ));
    }

    let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            return Err(classify_reqwest_transport_error_with_trace(
                "OpenAI-style response body failed",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                error,
                request_trace.as_ref(),
            ));
        }
        Err(_elapsed) => {
            return Err(timeout_transport_error_with_trace(
                "OpenAI-style response body read timed out",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                format!("timed out after {:?}", response_body_timeout()),
                request_trace.as_ref(),
            ));
        }
    };
    trace_response_body(request_trace.as_ref(), &body);
    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| invalid_response_error("invalid OpenAI-style JSON", error))?;
    parse_openai_response_with_transport_state(parsed)
        .map(|parsed| parsed.with_provider_request_id(provider_request_id))
}

async fn send_openai_images_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<Vec<ProviderGeneratedImage>> {
    let model_ref = provider_model_ref("openai", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai",
            Some(&model_ref),
            url.as_str(),
            "images_generations",
            &headers,
            &body,
        )
    });
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = send_openai_request(
        request.json(&body),
        "OpenAI Images request failed",
        "request_send",
        "openai",
        Some(&model_ref),
        Some(url.as_str()),
        false,
        request_trace.as_ref(),
    )
    .await?;
    trace_response_headers(
        request_trace.as_ref(),
        response.status(),
        response.headers(),
    );
    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        trace_response_body(request_trace.as_ref(), &body);
        return Err(classify_status_error_with_trace(
            "OpenAI Images request failed",
            "response_status",
            Some("openai"),
            Some(&model_ref),
            Some(url.as_str()),
            status,
            body,
            request_trace.as_ref(),
        ));
    }
    let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            return Err(classify_reqwest_transport_error_with_trace(
                "OpenAI Images response body failed",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                error,
                request_trace.as_ref(),
            ));
        }
        Err(_elapsed) => {
            return Err(timeout_transport_error_with_trace(
                "OpenAI Images response body read timed out",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                format!("timed out after {:?}", response_body_timeout()),
                request_trace.as_ref(),
            ));
        }
    };
    trace_response_body(request_trace.as_ref(), &body);
    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| invalid_response_error("invalid OpenAI Images JSON", error))?;
    parse_openai_images_response(parsed)
}

fn parse_openai_images_response(value: Value) -> Result<Vec<ProviderGeneratedImage>> {
    let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
        invalid_response_error("OpenAI Images response missing data", "missing data")
    })?;
    let mut images = Vec::new();
    for item in data {
        let b64 = item
            .get("b64_json")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                invalid_response_error(
                    "OpenAI Images response item missing b64_json",
                    "missing b64_json",
                )
            })?;
        let bytes = BASE64_STANDARD.decode(b64).map_err(|error| {
            invalid_response_error("invalid OpenAI Images base64 payload", error)
        })?;
        images.push(ProviderGeneratedImage { bytes, mime: None });
    }
    if images.is_empty() {
        return Err(invalid_response_error(
            "OpenAI Images response contained no images",
            "empty data",
        ));
    }
    Ok(images)
}

fn parse_openai_codex_image_generation_response_items(
    output_items: Vec<Value>,
) -> Result<Vec<ProviderGeneratedImage>> {
    let mut images = Vec::new();
    for item in output_items {
        if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
            continue;
        }
        let b64 = item.get("result").and_then(Value::as_str).ok_or_else(|| {
            invalid_response_error(
                "OpenAI Codex image_generation_call item missing result",
                "missing result",
            )
        })?;
        let bytes = BASE64_STANDARD.decode(b64).map_err(|error| {
            invalid_response_error(
                "invalid OpenAI Codex image_generation base64 payload",
                error,
            )
        })?;
        images.push(ProviderGeneratedImage {
            bytes,
            mime: Some("image/png".into()),
        });
    }
    if images.is_empty() {
        return Err(invalid_response_error(
            "OpenAI Codex image generation response contained no completed images",
            "missing completed image_generation_call",
        ));
    }
    Ok(images)
}

async fn send_openai_compact_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<Vec<Value>> {
    let model_ref = provider_model_ref("openai", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai",
            Some(&model_ref),
            url.as_str(),
            OPENAI_RESPONSES_COMPACT_ENDPOINT_KIND,
            &headers,
            &body,
        )
    });
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = send_openai_request(
        request.json(&body),
        "OpenAI compact request failed",
        "request_send",
        "openai",
        Some(&model_ref),
        Some(url.as_str()),
        true,
        request_trace.as_ref(),
    )
    .await?;
    trace_response_headers(
        request_trace.as_ref(),
        response.status(),
        response.headers(),
    );

    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        trace_response_body(request_trace.as_ref(), &body);
        return Err(classify_status_error_with_trace(
            "OpenAI compact request failed",
            "response_status",
            Some("openai"),
            Some(&model_ref),
            Some(url.as_str()),
            status,
            body,
            request_trace.as_ref(),
        ));
    }

    let response_body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            return Err(classify_reqwest_transport_error_with_trace(
                "OpenAI compact response body failed",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                error,
                request_trace.as_ref(),
            ));
        }
        Err(_elapsed) => {
            return Err(timeout_transport_error_with_trace(
                "OpenAI compact response body read timed out",
                "response_body",
                "openai",
                Some(&model_ref),
                Some(url.as_str()),
                format!("timed out after {:?}", response_body_timeout()),
                request_trace.as_ref(),
            ));
        }
    };
    trace_response_body(request_trace.as_ref(), &response_body);
    let parsed: Value = serde_json::from_str(&response_body)
        .map_err(|error| invalid_response_error("invalid OpenAI compact JSON", error))?;
    let output = parsed
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI compact response did not contain output array",
                "missing output array",
            )
        })?;
    Ok(output
        .into_iter()
        .map(|item| canonicalize_openai_provider_item(&item))
        .collect())
}

async fn send_openai_responses_streaming_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<ParsedOpenAiResponse> {
    let model_ref = provider_model_ref("openai-codex", &body);
    let request_trace = trace.and_then(|trace| {
        trace.begin_request(
            agent_id,
            "openai-codex",
            Some(&model_ref),
            url.as_str(),
            "responses_streaming",
            &headers,
            &body,
        )
    });
    let mut request = client.post(&url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = send_openai_request(
        request.json(&body),
        "OpenAI-style streaming request failed",
        "streaming_request_send",
        "openai-codex",
        Some(&model_ref),
        Some(url.as_str()),
        false,
        request_trace.as_ref(),
    )
    .await?;
    trace_response_headers(
        request_trace.as_ref(),
        response.status(),
        response.headers(),
    );
    let provider_request_id = provider_request_id_from_headers(response.headers());

    if !response.status().is_success() {
        let status = response.status();
        let body = match tokio::time::timeout(response_body_timeout(), response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        trace_response_body(request_trace.as_ref(), &body);
        return Err(classify_status_error_with_trace(
            openai_codex_status_error_context(status),
            "response_status",
            Some("openai-codex"),
            Some(&model_ref),
            Some(url.as_str()),
            status,
            body,
            request_trace.as_ref(),
        ));
    }

    let terminal_response =
        read_openai_streaming_response(response, request_trace.as_ref()).await?;
    parse_openai_response_with_transport_state(terminal_response)
        .map(|parsed| parsed.with_provider_request_id(provider_request_id))
}

async fn send_openai_codex_image_generation_request(
    client: &Client,
    url: String,
    body: Value,
    headers: Vec<(&str, String)>,
    trace: Option<&ProviderHttpTrace>,
    agent_id: Option<&str>,
) -> Result<Vec<ProviderGeneratedImage>> {
    let terminal_response =
        send_openai_responses_streaming_request(client, url, body, headers, trace, agent_id)
            .await?;
    parse_openai_codex_image_generation_response_items(terminal_response.output_items)
}

fn openai_codex_auth_error(
    stage: &str,
    model_ref: &str,
    source_chain: Vec<String>,
    message: &str,
) -> anyhow::Error {
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        None,
        Some(ProviderTransportDiagnostics {
            stage: stage.into(),
            provider: Some("openai-codex".into()),
            model_ref: Some(model_ref.into()),
            url: None,
            status: None,
            reqwest: None,
            http_trace: None,
            source_chain,
        }),
        message,
    )
}

fn openai_codex_status_error_context(status: reqwest::StatusCode) -> &'static str {
    if matches!(
        status,
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        "OpenAI Codex authentication failed; run Holon onboarding login for openai-codex again, or run `codex login` if intentionally using external CLI credentials."
    } else {
        "OpenAI-style streaming request failed"
    }
}

async fn send_openai_request(
    mut request: RequestBuilder,
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    enforce_full_request_deadline: bool,
    trace: Option<&ProviderHttpTraceRequest>,
) -> Result<Response> {
    let timeout = request_send_timeout();
    if enforce_full_request_deadline {
        request = request.timeout(timeout);
        return request.send().await.map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                context, stage, provider, model_ref, url, error, trace,
            )
        });
    }
    tokio::time::timeout(timeout, request.send())
        .await
        .map_err(|_| {
            timeout_transport_error_with_trace(
                context,
                stage,
                provider,
                model_ref,
                url,
                format!("request_send_timeout_ms={}", timeout.as_millis()),
                trace,
            )
        })?
        .map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                context, stage, provider, model_ref, url, error, trace,
            )
        })
}

async fn read_openai_streaming_response(
    response: Response,
    trace: Option<&ProviderHttpTraceRequest>,
) -> Result<Value> {
    let idle_timeout = stream_idle_timeout();
    read_openai_streaming_response_with_timeout(response, idle_timeout, trace).await
}

async fn read_openai_streaming_response_with_timeout(
    response: Response,
    idle_timeout: Duration,
    trace: Option<&ProviderHttpTraceRequest>,
) -> Result<Value> {
    const MAX_STREAMED_OUTPUT_ITEMS: usize = 128;

    let mut response = response;
    let mut pending = String::new();
    let mut data_lines = Vec::new();
    let mut streamed_output_items = Vec::new();

    while let Some(chunk) = tokio::time::timeout(idle_timeout, response.chunk())
        .await
        .map_err(|_| {
            timeout_transport_error_with_trace(
                "OpenAI-style streaming response body timed out",
                "streaming_response_body",
                "openai-codex",
                None,
                None,
                format!(
                    "timed out waiting for SSE chunk after {} ms",
                    idle_timeout.as_millis()
                ),
                trace,
            )
        })?
        .map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                "OpenAI-style streaming response body failed",
                "streaming_response_body",
                "openai-codex",
                None,
                None,
                error,
                trace,
            )
        })?
    {
        trace_stream_chunk(trace, &chunk);
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline_idx) = pending.find('\n') {
            let mut line = pending[..newline_idx].to_string();
            pending.drain(..=newline_idx);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                match consume_openai_sse_event(&mut data_lines)? {
                    StreamingSseEvent::Continue => {}
                    StreamingSseEvent::OutputItem(item) => push_streamed_output_item(
                        &mut streamed_output_items,
                        item,
                        MAX_STREAMED_OUTPUT_ITEMS,
                    )?,
                    StreamingSseEvent::Incomplete(response) => {
                        return recover_openai_incomplete_response(response, &streamed_output_items)
                    }
                    StreamingSseEvent::Done => return Err(early_done_protocol_violation_error()),
                    StreamingSseEvent::Terminal(response) => {
                        let response =
                            finalize_openai_terminal_response(response, &streamed_output_items);
                        trace_stream_terminal(trace, &response);
                        return Ok(response);
                    }
                }
                continue;
            }
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start().to_string());
            }
        }
    }

    if !pending.is_empty() {
        let line = pending.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }
    match consume_openai_sse_event(&mut data_lines)? {
        StreamingSseEvent::Continue => {}
        StreamingSseEvent::OutputItem(item) => {
            push_streamed_output_item(&mut streamed_output_items, item, MAX_STREAMED_OUTPUT_ITEMS)?
        }
        StreamingSseEvent::Incomplete(response) => {
            return recover_openai_incomplete_response(response, &streamed_output_items)
        }
        StreamingSseEvent::Done => return Err(early_done_protocol_violation_error()),
        StreamingSseEvent::Terminal(response) => {
            let response = finalize_openai_terminal_response(response, &streamed_output_items);
            trace_stream_terminal(trace, &response);
            return Ok(response);
        }
    }

    Err(invalid_response_error(
        "OpenAI-style streaming response did not contain a terminal response event",
        "missing terminal response",
    ))
}

fn model_from_request(body: &Value) -> &str {
    body.get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn provider_model_ref(provider: &str, body: &Value) -> String {
    format!("{provider}/{}", model_from_request(body))
}

fn early_done_protocol_violation_error() -> anyhow::Error {
    invalid_response_error(
        "OpenAI-style streaming response ended before a terminal response event",
        "[DONE] observed before terminal response",
    )
}

enum StreamingSseEvent {
    Continue,
    OutputItem(Value),
    Incomplete(Value),
    Done,
    Terminal(Value),
}

fn push_streamed_output_item(
    streamed_output_items: &mut Vec<Value>,
    item: Value,
    max_items: usize,
) -> Result<()> {
    if streamed_output_items.len() >= max_items {
        return Err(invalid_response_error(
            "OpenAI-style streaming response emitted too many output items",
            format!("received more than {max_items} streamed output items"),
        ));
    }
    streamed_output_items.push(item);
    Ok(())
}

fn consume_openai_sse_event(data_lines: &mut Vec<String>) -> Result<StreamingSseEvent> {
    if data_lines.is_empty() {
        return Ok(StreamingSseEvent::Continue);
    }

    let payload = data_lines.join("\n");
    data_lines.clear();
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Ok(StreamingSseEvent::Continue);
    }
    if trimmed == "[DONE]" {
        return Ok(StreamingSseEvent::Done);
    }

    let event: Value = serde_json::from_str(trimmed)
        .map_err(|error| invalid_response_error("invalid OpenAI-style streaming JSON", error))?;

    if event.get("type").and_then(Value::as_str) == Some("error") {
        return Err(classify_openai_streaming_error(
            "OpenAI-style streaming response reported an error event",
            event.get("error"),
            Some(&event),
        ));
    }

    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type == "response.output_item.done" {
        if let Some(item) = event.get("item") {
            return Ok(StreamingSseEvent::OutputItem(item.clone()));
        }
        return Err(invalid_response_error(
            "OpenAI-style streaming response output item event was missing item",
            "missing item",
        ));
    }
    if let Some(response) = event.get("response") {
        let status = response.get("status").and_then(Value::as_str);
        if event_type == "response.completed" || status == Some("completed") {
            return Ok(StreamingSseEvent::Terminal(response.clone()));
        }
        if event_type == "response.failed" || status == Some("failed") {
            return Err(classify_openai_streaming_error(
                "OpenAI-style streaming response failed",
                response.get("error"),
                Some(response),
            ));
        }
        if event_type == "response.incomplete" || status == Some("incomplete") {
            if openai_incomplete_reason(response) == Some("max_output_tokens") {
                return Ok(StreamingSseEvent::Incomplete(response.clone()));
            }
            return Err(classify_openai_incomplete_response(response));
        }
        if status == Some("cancelled") {
            return Err(classify_openai_incomplete_response(response));
        }
    }

    Ok(StreamingSseEvent::Continue)
}

fn recover_openai_incomplete_response(
    response: Value,
    streamed_output_items: &[Value],
) -> Result<Value> {
    let response = finalize_openai_terminal_response(response, streamed_output_items);
    let has_output = response
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| !output.is_empty());
    if has_output {
        Ok(response)
    } else {
        Err(classify_openai_incomplete_response(&response))
    }
}

fn finalize_openai_terminal_response(
    mut response: Value,
    streamed_output_items: &[Value],
) -> Value {
    let has_output = response
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| !output.is_empty());
    if has_output || streamed_output_items.is_empty() {
        return response;
    }

    if let Some(object) = response.as_object_mut() {
        object.insert(
            "output".to_string(),
            Value::Array(streamed_output_items.to_vec()),
        );
    }
    response
}

fn classify_openai_streaming_error(
    context: &str,
    error: Option<&Value>,
    response: Option<&Value>,
) -> anyhow::Error {
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .and_then(|response| response.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown streaming error");
    let classification = match code {
        Some("rate_limit_exceeded") => ProviderFailureClassification {
            kind: ProviderFailureKind::RateLimited,
            disposition: RetryDisposition::Retryable,
        },
        Some("server_error" | "service_unavailable" | "server_is_overloaded" | "slow_down") => {
            ProviderFailureClassification {
                kind: ProviderFailureKind::ServerError,
                disposition: RetryDisposition::Retryable,
            }
        }
        Some("insufficient_quota") => ProviderFailureClassification {
            kind: ProviderFailureKind::AuthError,
            disposition: RetryDisposition::FailFast,
        },
        Some("context_length_exceeded" | "invalid_prompt") => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        _ => ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
    };
    let detail = code
        .map(|code| format!("{code}: {message}"))
        .unwrap_or_else(|| message.to_string());
    provider_transport_error(classification, None, None, format!("{context}: {detail}"))
}

fn classify_openai_incomplete_response(response: &Value) -> anyhow::Error {
    let reason = openai_incomplete_reason(response)
        .or_else(|| response.get("status").and_then(Value::as_str))
        .unwrap_or("unknown");
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        None,
        None,
        format!("OpenAI-style streaming response did not complete successfully: {reason}"),
    )
}

fn openai_incomplete_reason(response: &Value) -> Option<&str> {
    response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
}

#[allow(dead_code)]
pub(crate) fn parse_openai_response(response: Value) -> Result<ProviderTurnResponse> {
    parse_openai_response_with_transport_state(response).map(|parsed| parsed.response)
}

fn parse_openai_response_with_transport_state(response: Value) -> Result<ParsedOpenAiResponse> {
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_response_error(
                "OpenAI-style response did not contain an output array",
                "missing output array",
            )
        })?;
    let output_items = output
        .iter()
        .map(canonicalize_openai_provider_item)
        .collect::<Vec<_>>();
    let mut blocks = Vec::new();

    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for content_item in content {
                        match content_item.get("type").and_then(Value::as_str) {
                            Some("output_text") | Some("text") | Some("input_text") => {
                                if let Some(text) = content_item.get("text").and_then(Value::as_str)
                                {
                                    blocks.push(ModelBlock::Text {
                                        text: text.to_string(),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("function_call") => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_response_error(
                            "function_call item did not include call_id",
                            "missing call_id",
                        )
                    })?;
                let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "function_call item did not include name",
                        "missing name",
                    )
                })?;
                let input = match item.get("arguments") {
                    Some(Value::String(arguments)) if !arguments.trim().is_empty() => {
                        serde_json::from_str(arguments).map_err(|error| {
                            invalid_response_error("invalid function_call arguments", error)
                        })?
                    }
                    Some(Value::Object(arguments)) => Value::Object(arguments.clone()),
                    _ => json!({}),
                };
                blocks.push(ModelBlock::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input,
                });
            }
            Some("custom_tool_call") => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_response_error(
                            "custom_tool_call item did not include call_id",
                            "missing call_id",
                        )
                    })?;
                let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "custom_tool_call item did not include name",
                        "missing name",
                    )
                })?;
                let input = item.get("input").and_then(Value::as_str).ok_or_else(|| {
                    invalid_response_error(
                        "custom_tool_call item did not include string input",
                        "missing input",
                    )
                })?;
                blocks.push(ModelBlock::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: Value::String(input.to_string()),
                });
            }
            _ => {}
        }
    }

    if blocks.is_empty() {
        return Err(invalid_response_error(
            "OpenAI-style response contained no supported content blocks",
            "empty supported block set",
        ));
    }

    let usage = response.get("usage").and_then(Value::as_object);
    let cache_usage = usage.map(|usage| ProviderCacheUsage {
        read_input_tokens: usage
            .get("input_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(Value::as_object)
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .unwrap_or(0),
        creation_input_tokens: 0,
    });
    Ok(ParsedOpenAiResponse {
        response: ProviderTurnResponse {
            blocks,
            stop_reason: response
                .get("incomplete_details")
                .and_then(|details| details.get("reason"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    response
                        .get("status")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .or_else(|| {
                    response
                        .get("stop_reason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }),
            input_tokens: usage
                .and_then(|usage| usage.get("input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_usage,
            provider_message_id: response_id.clone(),
            provider_request_id: None,
            request_diagnostics: None,
        },
        response_id,
        output_items,
    })
}

#[allow(dead_code)]
fn unsupported_streaming_transport_error(provider_name: &str) -> anyhow::Error {
    provider_transport_error(
        ProviderFailureClassification {
            kind: ProviderFailureKind::UnsupportedTransport,
            disposition: RetryDisposition::FailFast,
        },
        None,
        None,
        format!("{provider_name} requires a streaming transport contract"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_openai_codex_image_generation_request, build_openai_responses_request,
        chat_completions_url, choose_openai_codex_credential, consume_openai_sse_event,
        incremental_diagnostics, latest_openai_compaction_index, native_web_search_diagnostics,
        openai_compaction_trigger_for_request_plan, openai_compaction_trigger_for_window,
        openai_provider_window_compaction_candidate,
        parse_openai_codex_image_generation_response_items, plan_openai_responses_request,
        resolve_openai_codex_credential, CredentialStoreRefreshLock, OpenAiCodexProvider,
        OpenAiCompactionPolicy, OpenAiContinuationState, OpenAiProviderWindow, OpenAiRequestPlan,
        OpenAiRequestShape, OpenAiResponsesTransportContract, ToolSchemaContract,
    };
    use crate::auth::CodexCliCredential;
    use crate::config::{
        load_credential_store_at, save_credential_store_at, CredentialKind, CredentialProfileFile,
        CredentialSource, CredentialStoreFile, ProviderAuthConfig, ProviderEndpointId, ProviderId,
        ProviderRuntimeConfig, ProviderTransportKind, OPENAI_CODEX_CREDENTIAL_PROFILE,
    };
    use crate::provider::retry::{classify_provider_error, ProviderFailureKind, RetryDisposition};
    use crate::provider::{
        ConversationMessage, ProviderGenerateImageRequest, ProviderJsonSchemaResponseFormat,
        ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest, ProviderResponseFormatRequest,
        ProviderTurnRequest,
    };
    use base64::prelude::BASE64_STANDARD;
    use base64::Engine;
    use chrono::Utc;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    static CODEX_REFRESH_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.as_ref() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn encode_segment(value: serde_json::Value) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    }

    fn make_token(payload: serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            encode_segment(json!({"alg": "none"})),
            encode_segment(payload),
            encode_segment(json!("sig"))
        )
    }

    fn test_openai_codex_config(credential: Option<String>) -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            id: ProviderId::openai_codex(),
            route_provider: ProviderId::openai_codex(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(OPENAI_CODEX_CREDENTIAL_PROFILE.into()),
                external: Some("codex_cli".into()),
            },
            credential,
            credential_store_path: None,
            codex_home: Some(PathBuf::from("/tmp/codex-home")),
            originator: Some("codex_cli_rs".into()),
            reasoning_effort: Some("low".into()),
            context_management: Default::default(),
            builtin_web_search: None,
        }
    }

    #[test]
    fn chat_completions_url_accepts_openai_compatible_base_urls() {
        assert_eq!(
            chat_completions_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://qianfan.baidubce.com/v2"),
            "https://qianfan.baidubce.com/v2/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://ark.ap-southeast.bytepluses.com/api/v3"),
            "https://ark.ap-southeast.bytepluses.com/api/v3/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.z.ai/api/paas/v4"),
            "https://api.z.ai/api/paas/v4/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://proxy.example/chat/completions"),
            "https://proxy.example/chat/completions"
        );
    }

    #[test]
    fn openai_codex_resolves_holon_oauth_profile_before_cli_files() {
        let credential_material = json!({
            "tokens": {
                "access_token": make_token(json!({
                    "exp": 1_900_000_000,
                    "chatgpt_account_id": "acct_profile"
                })),
                "refresh_token": "refresh",
                "account_id": "acct_profile"
            }
        })
        .to_string();
        let provider_config = test_openai_codex_config(Some(credential_material));

        let credential = resolve_openai_codex_credential(
            &provider_config,
            provider_config.codex_home.as_ref().unwrap(),
        )
        .expect("Holon profile credential should resolve");

        assert_eq!(credential.account_id, "acct_profile");
        assert_eq!(
            credential.source,
            format!("credential_profile:{OPENAI_CODEX_CREDENTIAL_PROFILE}")
        );
    }

    #[test]
    fn openai_codex_prefers_fresher_cli_credential_over_stale_profile() {
        let profile = CodexCliCredential {
            access_token: "profile-access".into(),
            account_id: "acct_profile".into(),
            expires_at: chrono::DateTime::from_timestamp(1_900_000_000, 0),
            refreshed_at: chrono::DateTime::from_timestamp(1_800_000_000, 0),
            source: format!("credential_profile:{OPENAI_CODEX_CREDENTIAL_PROFILE}"),
        };
        let cli = CodexCliCredential {
            access_token: "cli-access".into(),
            account_id: "acct_cli".into(),
            expires_at: chrono::DateTime::from_timestamp(1_910_000_000, 0),
            refreshed_at: chrono::DateTime::from_timestamp(1_810_000_000, 0),
            source: "keychain".into(),
        };

        let credential = choose_openai_codex_credential(Some(profile), Some(cli))
            .expect("credential should resolve");

        assert_eq!(credential.access_token, "cli-access");
        assert_eq!(credential.source, "keychain");
    }

    #[tokio::test]
    async fn openai_codex_refreshes_and_persists_holon_oauth_profile() {
        let server = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "access_token": make_token(json!({
                        "exp": 1_900_000_000,
                        "chatgpt_account_id": "acct_profile"
                    })),
                    "refresh_token": "rotated-refresh"
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        });
        let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::set(
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
            format!("http://{addr}/oauth/token"),
        );

        let home = tempfile::tempdir().unwrap();
        let credential_store_path = home.path().join("credentials.json");
        let expiring_material = json!({
            "tokens": {
                "access_token": make_token(json!({
                    "exp": Utc::now().timestamp() + 30,
                    "chatgpt_account_id": "acct_profile"
                })),
                "refresh_token": "old-refresh",
                "account_id": "acct_profile"
            }
        })
        .to_string();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            OPENAI_CODEX_CREDENTIAL_PROFILE.to_string(),
            CredentialProfileFile {
                kind: CredentialKind::OAuth,
                material: expiring_material.clone(),
            },
        );
        save_credential_store_at(&credential_store_path, &CredentialStoreFile { profiles })
            .unwrap();

        let mut provider_config = test_openai_codex_config(Some(expiring_material));
        provider_config.credential_store_path = Some(credential_store_path.clone());
        let provider = OpenAiCodexProvider::from_runtime_config(
            &provider_config,
            "gpt-codex-test",
            1024,
            home.path(),
            true,
        )
        .unwrap();

        let credential = provider.resolve_fresh_credential().await.unwrap();
        assert_eq!(credential.account_id, "acct_profile");

        let store = load_credential_store_at(&credential_store_path).unwrap();
        let material = &store.profiles[OPENAI_CODEX_CREDENTIAL_PROFILE].material;
        assert!(material.contains("rotated-refresh"));
        assert!(!material.contains("old-refresh"));
        handle.abort();
    }

    #[tokio::test]
    async fn openai_codex_auth_failure_forces_profile_refresh_even_when_jwt_is_not_expiring() {
        let server = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "access_token": make_token(json!({
                        "exp": Utc::now().timestamp() + 3600,
                        "chatgpt_account_id": "acct_profile"
                    })),
                    "refresh_token": "rotated-refresh"
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        });
        let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::set(
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
            format!("http://{addr}/oauth/token"),
        );

        let home = tempfile::tempdir().unwrap();
        let credential_store_path = home.path().join("credentials.json");
        let valid_but_invalidated_material = json!({
            "tokens": {
                "access_token": make_token(json!({
                    "exp": Utc::now().timestamp() + 3600,
                    "chatgpt_account_id": "acct_profile"
                })),
                "refresh_token": "old-refresh",
                "account_id": "acct_profile"
            }
        })
        .to_string();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            OPENAI_CODEX_CREDENTIAL_PROFILE.to_string(),
            CredentialProfileFile {
                kind: CredentialKind::OAuth,
                material: valid_but_invalidated_material.clone(),
            },
        );
        save_credential_store_at(&credential_store_path, &CredentialStoreFile { profiles })
            .unwrap();

        let mut provider_config = test_openai_codex_config(Some(valid_but_invalidated_material));
        provider_config.credential_store_path = Some(credential_store_path.clone());
        let provider = OpenAiCodexProvider::from_runtime_config(
            &provider_config,
            "gpt-codex-test",
            1024,
            home.path(),
            true,
        )
        .unwrap();

        let old_credential = provider.resolve_fresh_credential().await.unwrap();
        assert!(old_credential.source.starts_with("credential_profile:"));

        let refreshed = provider
            .refresh_after_auth_failure(&old_credential)
            .await
            .unwrap()
            .expect("profile auth failure should force refresh");

        assert_eq!(refreshed.account_id, "acct_profile");
        let store = load_credential_store_at(&credential_store_path).unwrap();
        let material = &store.profiles[OPENAI_CODEX_CREDENTIAL_PROFILE].material;
        assert!(material.contains("rotated-refresh"));
        assert!(!material.contains("old-refresh"));
        handle.abort();
    }

    #[tokio::test]
    async fn openai_codex_refresh_fails_without_access_token() {
        let server = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "refresh_token": "rotated-refresh"
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        });
        let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::set(
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
            format!("http://{addr}/oauth/token"),
        );

        let home = tempfile::tempdir().unwrap();
        let credential_store_path = home.path().join("credentials.json");
        let expiring_material = json!({
            "tokens": {
                "access_token": make_token(json!({
                    "exp": Utc::now().timestamp() + 30,
                    "chatgpt_account_id": "acct_profile"
                })),
                "refresh_token": "old-refresh",
                "account_id": "acct_profile"
            }
        })
        .to_string();
        let mut profiles = BTreeMap::new();
        profiles.insert(
            OPENAI_CODEX_CREDENTIAL_PROFILE.to_string(),
            CredentialProfileFile {
                kind: CredentialKind::OAuth,
                material: expiring_material.clone(),
            },
        );
        save_credential_store_at(&credential_store_path, &CredentialStoreFile { profiles })
            .unwrap();

        let mut provider_config = test_openai_codex_config(Some(expiring_material));
        provider_config.credential_store_path = Some(credential_store_path.clone());
        let provider = OpenAiCodexProvider::from_runtime_config(
            &provider_config,
            "gpt-codex-test",
            1024,
            home.path(),
            true,
        )
        .unwrap();

        let error = provider
            .resolve_fresh_credential()
            .await
            .expect_err("refresh without an access token should fail");
        assert!(
            error
                .to_string()
                .contains("refresh response did not include an access token"),
            "{error}"
        );
        let store = load_credential_store_at(&credential_store_path).unwrap();
        let material = &store.profiles[OPENAI_CODEX_CREDENTIAL_PROFILE].material;
        assert!(material.contains("old-refresh"));
        assert!(!material.contains("rotated-refresh"));
        handle.abort();
    }

    #[test]
    fn openai_codex_refresh_lock_uses_owner_only_permissions() {
        let home = tempfile::tempdir().unwrap();
        let lock_path = home.path().join("credentials.json.lock");
        let lock = CredentialStoreRefreshLock::acquire(&lock_path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        drop(lock);
        assert!(!lock_path.exists());
    }

    #[test]
    fn openai_streaming_error_event_classifies_transient_server_codes_as_retryable() {
        for code in [
            "server_error",
            "service_unavailable",
            "server_is_overloaded",
            "slow_down",
        ] {
            let mut data_lines = vec![json!({
                "type": "error",
                "error": {
                    "code": code,
                    "message": "temporary server failure"
                }
            })
            .to_string()];

            let error = match consume_openai_sse_event(&mut data_lines) {
                Ok(_) => panic!("transient streaming error event should produce a provider error"),
                Err(error) => error,
            };
            let classification = classify_provider_error(&error);

            assert_eq!(classification.kind, ProviderFailureKind::ServerError);
            assert_eq!(classification.disposition, RetryDisposition::Retryable);
        }
    }

    #[test]
    fn openai_streaming_error_event_keeps_unknown_codes_fail_fast_contract() {
        let mut data_lines = vec![json!({
            "type": "error",
            "error": {
                "code": "unexpected_protocol_state",
                "message": "unexpected stream shape"
            }
        })
        .to_string()];

        let error = match consume_openai_sse_event(&mut data_lines) {
            Ok(_) => panic!("unknown streaming error event should produce a provider error"),
            Err(error) => error,
        };
        let classification = classify_provider_error(&error);

        assert_eq!(classification.kind, ProviderFailureKind::ContractError);
        assert_eq!(classification.disposition, RetryDisposition::FailFast);
    }

    #[test]
    fn openai_responses_request_lowers_native_web_search_tool() {
        let mut request = ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserText("search the web".into())],
            vec![],
        );
        request.native_web_search = Some(ProviderNativeWebSearchRequest {
            kind: ProviderNativeWebSearchKind::OpenAi,
            provider_id: "openai_native".into(),
            provider_model_ref: "openai/gpt-test".into(),
            advertised_tool_type: "web_search_preview".into(),
            backend_kind: "openai_web_search".into(),
            max_results: Some(5),
        });

        let body = build_openai_responses_request(
            "gpt-test",
            1024,
            &request,
            OpenAiResponsesTransportContract::StandardJson,
            ToolSchemaContract::Strict,
            None,
            None,
        )
        .expect("openai responses request should build");

        assert!(body["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .any(|tool| tool == &json!({ "type": "web_search_preview" })));
    }

    #[test]
    fn openai_responses_request_lowers_xai_native_search_tools_and_reasoning() {
        let mut request = ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserText("search X and the web".into())],
            vec![],
        );
        request.native_web_search = Some(ProviderNativeWebSearchRequest {
            kind: ProviderNativeWebSearchKind::Xai,
            provider_id: "xai".into(),
            provider_model_ref: "xai/grok-4-fast".into(),
            advertised_tool_type: "web_search".into(),
            backend_kind: "xai_web_search_x_search".into(),
            max_results: Some(5),
        });

        let body = build_openai_responses_request(
            "grok-4-fast",
            1024,
            &request,
            OpenAiResponsesTransportContract::StandardJson,
            ToolSchemaContract::Strict,
            Some("medium"),
            None,
        )
        .expect("xAI responses request should build");

        let tools = body["tools"].as_array().expect("tools should be an array");
        assert!(tools
            .iter()
            .any(|tool| tool == &json!({ "type": "web_search" })));
        assert!(tools
            .iter()
            .any(|tool| tool == &json!({ "type": "x_search" })));
        assert_eq!(body["reasoning"]["effort"], json!("medium"));

        let diagnostics = native_web_search_diagnostics(&request)
            .expect("native web search diagnostics should be recorded");
        assert!(diagnostics.lowered);
        assert_eq!(diagnostics.kind, ProviderNativeWebSearchKind::Xai);
        assert_eq!(diagnostics.fallback_reason, None);
    }

    #[test]
    fn openai_responses_request_lowers_json_schema_response_format() {
        let mut request = ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserText("return json".into())],
            vec![],
        );
        request.response_format = Some(ProviderResponseFormatRequest::JsonSchema(
            ProviderJsonSchemaResponseFormat {
                name: "answer_v1".into(),
                strict: true,
                schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["answer"],
                    "properties": {
                        "answer": { "type": "string" }
                    }
                }),
            },
        ));

        let body = build_openai_responses_request(
            "gpt-test",
            1024,
            &request,
            OpenAiResponsesTransportContract::StandardJson,
            ToolSchemaContract::Strict,
            None,
            None,
        )
        .expect("openai responses request should build");

        assert!(body.get("response_format").is_none());
        assert_eq!(body["text"]["format"]["type"], json!("json_schema"));
        assert_eq!(body["text"]["format"]["name"], json!("answer_v1"));
        assert_eq!(body["text"]["format"]["strict"], json!(true));
        assert_eq!(
            body["text"]["format"]["schema"]["properties"]["answer"]["type"],
            json!("string")
        );
    }

    #[test]
    fn openai_codex_responses_request_lowers_native_web_search_tool() {
        let mut request = ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserText("search the web".into())],
            vec![],
        );
        request.native_web_search = Some(ProviderNativeWebSearchRequest {
            kind: ProviderNativeWebSearchKind::OpenAi,
            provider_id: "openai_codex_native".into(),
            provider_model_ref: "openai-codex/gpt-codex-test".into(),
            advertised_tool_type: "web_search".into(),
            backend_kind: "openai_codex_web_search".into(),
            max_results: Some(5),
        });

        let body = build_openai_responses_request(
            "gpt-codex-test",
            1024,
            &request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
            Some("low"),
            None,
        )
        .expect("openai codex responses request should build");

        assert!(body["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .any(|tool| tool == &json!({ "type": "web_search" })));
        assert_eq!(body["stream"], json!(true));
    }

    #[test]
    fn openai_codex_image_generation_request_uses_hosted_tool() {
        let request = ProviderGenerateImageRequest {
            prompt: "draw a small holon".into(),
            size: Some("1024x1024".into()),
            background: Some("transparent".into()),
            output_format: Some("png".into()),
        };

        let body = build_openai_codex_image_generation_request("gpt-5.3-codex-spark", &request);

        assert_eq!(body["model"], json!("gpt-5.3-codex-spark"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["store"], json!(false));
        assert_eq!(
            body["input"][0]["content"][0],
            json!({
                "type": "input_text",
                "text": "draw a small holon",
            })
        );
        assert_eq!(
            body["tools"][0],
            json!({
                "type": "image_generation",
                "output_format": "png",
                "size": "1024x1024",
                "background": "transparent",
            })
        );
    }

    #[test]
    fn openai_codex_image_generation_response_parses_result_items() {
        let images = parse_openai_codex_image_generation_response_items(vec![
            json!({
                "type": "message",
                "content": [],
            }),
            json!({
                "type": "image_generation_call",
                "id": "ig_1",
                "status": "completed",
                "revised_prompt": "draw a tiny holon",
                "result": BASE64_STANDARD.encode(b"fake_png"),
            }),
        ])
        .expect("image_generation_call should parse");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].bytes, b"fake_png");
        assert_eq!(images[0].mime.as_deref(), Some("image/png"));
    }

    #[test]
    fn openai_codex_image_generation_response_accepts_done_item_with_generating_status() {
        let images = parse_openai_codex_image_generation_response_items(vec![json!({
            "type": "image_generation_call",
            "id": "ig_1",
            "status": "generating",
            "result": BASE64_STANDARD.encode(b"fake_png"),
        })])
        .expect("image_generation_call with a final result should parse");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].bytes, b"fake_png");
    }

    #[test]
    fn openai_responses_full_request_records_native_web_search_diagnostics() {
        let mut request = ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserText("search the web".into())],
            vec![],
        );
        request.native_web_search = Some(ProviderNativeWebSearchRequest {
            kind: ProviderNativeWebSearchKind::OpenAi,
            provider_id: "openai_codex_native".into(),
            provider_model_ref: "openai-codex/gpt-codex-test".into(),
            advertised_tool_type: "web_search".into(),
            backend_kind: "openai_codex_web_search".into(),
            max_results: Some(5),
        });

        let body = build_openai_responses_request(
            "gpt-codex-test",
            1024,
            &request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
            Some("low"),
            None,
        )
        .expect("openai codex responses request should build");
        let plan = plan_openai_responses_request(
            body,
            &request,
            &Arc::new(Mutex::new(OpenAiContinuationState::default())),
            false,
        )
        .expect("openai codex responses request should plan");

        let diagnostics = plan
            .diagnostics
            .native_web_search
            .expect("native web search diagnostics should be recorded");
        assert!(diagnostics.lowered);
        assert_eq!(diagnostics.advertised_tool_type, "web_search");
        assert_eq!(diagnostics.backend_kind, "openai_codex_web_search");
    }

    #[test]
    fn openai_codex_request_omits_reasoning_when_supports_reasoning_is_false() {
        // Negative-path guard test: when supports_reasoning=false, the provider
        // passes reasoning_effort=None to build_openai_responses_request, which
        // must produce a body with reasoning=null and no reasoning.effort field.
        let request = ProviderTurnRequest::plain(
            "system",
            vec![ConversationMessage::UserText("hello".into())],
            vec![],
        );

        // Guard suppressed path (supports_reasoning=false → None)
        let suppressed = build_openai_responses_request(
            "gpt-test",
            4096,
            &request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
            None,
            None,
        )
        .expect("suppressed reasoning request should build");
        assert_eq!(suppressed["reasoning"], json!(null));
        assert!(suppressed["reasoning"].get("effort").is_none());

        // Guard enabled path (supports_reasoning=true → Some)
        let enabled = build_openai_responses_request(
            "gpt-test",
            4096,
            &request,
            OpenAiResponsesTransportContract::CodexStreaming,
            ToolSchemaContract::Relaxed,
            Some("low"),
            None,
        )
        .expect("enabled reasoning request should build");
        assert_eq!(enabled["reasoning"]["effort"], json!("low"));
    }

    #[test]
    fn openai_provider_window_tracks_latest_compaction_item() {
        let items = vec![
            json!({ "type": "message", "role": "user" }),
            json!({ "type": "compaction", "encrypted_content": "first" }),
            json!({ "type": "message", "role": "user" }),
            json!({ "type": "compaction", "encrypted_content": "second" }),
        ];

        assert_eq!(latest_openai_compaction_index(&items), Some(3));
    }

    #[test]
    fn openai_compaction_trigger_skips_many_small_items_below_budget() {
        let request_shape = test_request_shape();
        let window = OpenAiProviderWindow {
            response_id: Some("resp_1".into()),
            request_shape: request_shape.clone(),
            items: (0..24)
                .map(|index| json!({ "type": "message", "content": format!("m{index}") }))
                .collect(),
            append_match_items: Vec::new(),
            latest_compaction_index: None,
            latest_input_tokens: 0,
            generation: 1,
        };

        assert!(openai_compaction_trigger_for_window(
            &window,
            &request_shape,
            OpenAiCompactionPolicy {
                trigger_input_tokens: 10_000,
            },
        )
        .is_none());
    }

    #[test]
    fn openai_compaction_candidate_allows_single_large_item() {
        let request_shape = test_request_shape();
        let window = OpenAiProviderWindow {
            response_id: Some("resp_1".into()),
            request_shape: request_shape.clone(),
            items: vec![json!({
                "type": "message",
                "content": "x".repeat(4096),
            })],
            append_match_items: Vec::new(),
            latest_compaction_index: None,
            latest_input_tokens: 0,
            generation: 1,
        };

        let trigger = openai_compaction_trigger_for_window(
            &window,
            &request_shape,
            OpenAiCompactionPolicy {
                trigger_input_tokens: 128,
            },
        )
        .expect("large item should reach token pressure");
        assert_eq!(trigger.reason, "estimated_window_pressure");

        let candidate = openai_provider_window_compaction_candidate(&window)
            .expect("single complete message should be compactable");
        assert_eq!(candidate.items.len(), 1);
    }

    #[test]
    fn openai_compaction_trigger_prefers_provider_usage_tokens() {
        let request_shape = test_request_shape();
        let window = OpenAiProviderWindow {
            response_id: Some("resp_1".into()),
            request_shape: request_shape.clone(),
            items: vec![json!({ "type": "message", "content": "small" })],
            append_match_items: Vec::new(),
            latest_compaction_index: None,
            latest_input_tokens: 512,
            generation: 1,
        };

        let trigger = openai_compaction_trigger_for_window(
            &window,
            &request_shape,
            OpenAiCompactionPolicy {
                trigger_input_tokens: 128,
            },
        )
        .expect("usage should reach token pressure");
        assert_eq!(trigger.reason, "token_budget_pressure");
        assert_eq!(trigger.estimated_input_tokens, None);
    }

    #[test]
    fn openai_compaction_trigger_skips_immediate_compacted_replay_before_usage() {
        let request_shape = test_request_shape();
        let previous = OpenAiProviderWindow {
            response_id: Some("resp_1".into()),
            request_shape: request_shape.clone(),
            items: vec![
                json!({ "type": "compaction", "encrypted_content": "opaque" }),
                json!({ "type": "message", "content": "recent" }),
            ],
            append_match_items: Vec::new(),
            latest_compaction_index: Some(0),
            latest_input_tokens: 0,
            generation: 1,
        };
        let plan = OpenAiRequestPlan {
            body: json!({ "model": "gpt-test", "input": [] }),
            scope: None,
            append_match_input: Vec::new(),
            provider_input: vec![json!({ "type": "message", "content": "continue" })],
            request_shape,
            diagnostics: incremental_diagnostics(
                "provider_window_compacted",
                "test",
                None,
                0,
                None,
                None,
                None,
                None,
            ),
        };

        assert!(openai_compaction_trigger_for_request_plan(
            &previous,
            &plan,
            OpenAiCompactionPolicy {
                trigger_input_tokens: 1,
            },
        )
        .is_none());
    }

    fn test_request_shape() -> OpenAiRequestShape {
        OpenAiRequestShape {
            wire_shape: json!({ "model": "gpt-test" }),
            prompt_frame: crate::provider::ProviderPromptFrame::plain("system"),
        }
    }
}
