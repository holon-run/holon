use std::{
    path::Path,
    sync::Mutex as StdMutex,
    time::{Duration as StdDuration, Instant, SystemTime},
};

use sha2::{Digest, Sha256};

use super::*;
use crate::{
    config::RuntimeModelCatalog,
    model_discovery::{
        discovery_cache_needs_refresh, discovery_cache_path, load_discovery_cache_at,
        provider_supports_model_discovery, refresh_provider_models, DEFAULT_DISCOVERY_CACHE_TTL,
    },
    provider::resolved_model_availability,
};

/// Upper bound on how long `/api/models` may serve a cached catalog snapshot
/// while credential material changes outside config reloads (OAuth refresh,
/// profile edits). Config replacement and discovery-cache rewrites invalidate
/// the snapshot immediately; this only bounds dynamic availability staleness.
const MODELS_SNAPSHOT_TTL: StdDuration = StdDuration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HandshakeResponse {
    pub ok: bool,
    pub protocol: HandshakeProtocol,
    pub auth: HandshakeAuth,
    pub capabilities: Vec<String>,
    pub runtime: HandshakeRuntime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<HandshakeLimits>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HandshakeLimits {
    pub prompt_body_max_bytes: u64,
    pub prompt_image_attachment_max_bytes: u64,
    pub prompt_file_attachment_max_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HandshakeProtocol {
    pub name: String,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HandshakeAuth {
    pub mode: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HandshakeRuntime {
    pub default_agent: String,
    pub workspace_dir: String,
    pub home_dir: String,
    pub listen: String,
    pub advertise_url: Option<String>,
}

pub async fn root(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    if accepts_html(&headers) {
        if let Some(response) = web_asset_response(&state, "index.html", &headers, false).await {
            return Ok(response);
        }
    }
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let default_agent = state
        .host
        .configured_default_agent_id()
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "default_agent": default_agent,
    }))
    .into_response())
}

pub async fn models_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    let started_at = Instant::now();
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let snapshot = state.models_cache.snapshot(&state.host).await?;
    Ok(models_snapshot_response(
        "/models", started_at, &headers, &snapshot,
    ))
}

pub async fn refresh_models_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    let started_at = Instant::now();
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let source = state.host.config();
    let mut config = (*source).clone();
    load_model_discovery_cache(&mut config).await?;
    let cache_path = discovery_cache_path(&config.home_dir);
    for provider in model_discovery_refresh_candidates(&config) {
        if let Err(error) = refresh_provider_models(&provider, &cache_path).await {
            warn!(
                provider = %provider.id.as_str(),
                error = %error,
                "explicit model discovery refresh failed"
            );
        }
    }

    load_model_discovery_cache(&mut config).await?;
    let snapshot = state.models_cache.replace(source, config).await?;
    Ok(models_snapshot_response(
        "/models/refresh",
        started_at,
        &headers,
        &snapshot,
    ))
}

async fn load_model_discovery_cache(
    config: &mut crate::config::AppConfig,
) -> Result<(), (StatusCode, Json<Value>)> {
    let cache_path = discovery_cache_path(&config.home_dir);
    config.model_discovery_cache =
        tokio::task::spawn_blocking(move || load_discovery_cache_at(&cache_path))
            .await
            .map_err(|error| error_response(error.into()))?
            .map_err(error_response)?;
    Ok(())
}

/// Cache key component for the on-disk discovery cache. Requests on a cache
/// hit skip both the 1 MiB JSON load and the catalog rebuild; a rewritten
/// file (refresh, rotation) changes length or mtime and invalidates the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiscoveryCacheStamp {
    len: u64,
    modified: Option<SystemTime>,
}

fn discovery_cache_stamp(home_dir: &Path) -> Option<DiscoveryCacheStamp> {
    let metadata = std::fs::metadata(discovery_cache_path(home_dir)).ok()?;
    Some(DiscoveryCacheStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

struct ModelsSnapshot {
    source_config: Arc<crate::config::AppConfig>,
    discovery_cache: Option<DiscoveryCacheStamp>,
    built_at: Instant,
    payload: Bytes,
    etag: HeaderValue,
}

/// Shared `/api/models` snapshot. The catalog sweep is CPU-bound and the body
/// is multi-megabyte, so a hit reuses both; misses build once under a
/// single-flight gate instead of stampeding the executor.
pub(crate) struct ModelsResponseCache {
    snapshot: StdMutex<Option<Arc<ModelsSnapshot>>>,
    build_gate: tokio::sync::Mutex<()>,
    ttl: StdDuration,
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
}

impl ModelsResponseCache {
    pub(crate) fn new() -> Self {
        Self {
            snapshot: StdMutex::new(None),
            build_gate: tokio::sync::Mutex::new(()),
            ttl: MODELS_SNAPSHOT_TTL,
            #[cfg(test)]
            builds: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    fn builds(&self) -> usize {
        self.builds.load(std::sync::atomic::Ordering::Acquire)
    }

    fn cached(
        &self,
        source_config: &Arc<crate::config::AppConfig>,
        discovery_cache: Option<DiscoveryCacheStamp>,
    ) -> Option<Arc<ModelsSnapshot>> {
        let guard = self
            .snapshot
            .lock()
            .expect("models snapshot cache poisoned");
        let snapshot = guard.as_ref()?;
        (Arc::ptr_eq(&snapshot.source_config, source_config)
            && snapshot.discovery_cache == discovery_cache
            && snapshot.built_at.elapsed() < self.ttl)
            .then(|| snapshot.clone())
    }

    async fn snapshot(
        &self,
        host: &RuntimeHost,
    ) -> Result<Arc<ModelsSnapshot>, (StatusCode, Json<Value>)> {
        let source = host.config();
        let stamp = discovery_cache_stamp(&source.home_dir);
        if let Some(snapshot) = self.cached(&source, stamp) {
            return Ok(snapshot);
        }

        let _build = self.build_gate.lock().await;
        // Another request may have built while this one waited on the gate.
        let source = host.config();
        let stamp = discovery_cache_stamp(&source.home_dir);
        if let Some(snapshot) = self.cached(&source, stamp) {
            return Ok(snapshot);
        }

        let mut config = (*source).clone();
        load_model_discovery_cache(&mut config).await?;
        let snapshot = Arc::new(build_models_snapshot(source, config, stamp).await?);
        #[cfg(test)]
        self.builds
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        *self
            .snapshot
            .lock()
            .expect("models snapshot cache poisoned") = Some(snapshot.clone());
        Ok(snapshot)
    }

    async fn replace(
        &self,
        source_config: Arc<crate::config::AppConfig>,
        config: crate::config::AppConfig,
    ) -> Result<Arc<ModelsSnapshot>, (StatusCode, Json<Value>)> {
        let _build = self.build_gate.lock().await;
        let stamp = discovery_cache_stamp(&source_config.home_dir);
        let snapshot = Arc::new(build_models_snapshot(source_config, config, stamp).await?);
        #[cfg(test)]
        self.builds
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        *self
            .snapshot
            .lock()
            .expect("models snapshot cache poisoned") = Some(snapshot.clone());
        Ok(snapshot)
    }
}

async fn build_models_snapshot(
    source_config: Arc<crate::config::AppConfig>,
    config: crate::config::AppConfig,
    discovery_cache: Option<DiscoveryCacheStamp>,
) -> Result<ModelsSnapshot, (StatusCode, Json<Value>)> {
    tokio::task::spawn_blocking(move || {
        build_models_snapshot_blocking(source_config, config, discovery_cache)
    })
    .await
    .map_err(|error| error_response(error.into()))?
    .map_err(error_response)
}

fn build_models_snapshot_blocking(
    source_config: Arc<crate::config::AppConfig>,
    config: crate::config::AppConfig,
    discovery_cache: Option<DiscoveryCacheStamp>,
) -> anyhow::Result<ModelsSnapshot> {
    let available_models = RuntimeModelCatalog::from_config(&config).available_models();
    let model_availability = resolved_model_availability(&config);
    let payload = serde_json::to_vec(&json!({
        "available_models": available_models,
        "model_availability": model_availability,
        "model_discovery_cache": Vec::<crate::model_discovery::ModelDiscoveryCacheStatus>::new(),
    }))?;
    let etag = HeaderValue::from_str(&entity_tag(&payload))
        .expect("sha256 entity tag is a valid header value");
    Ok(ModelsSnapshot {
        source_config,
        discovery_cache,
        built_at: Instant::now(),
        payload: Bytes::from(payload),
        etag,
    })
}

fn entity_tag(payload: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(payload);
    let mut etag = String::with_capacity(digest.len() * 2 + 2);
    etag.push('"');
    for byte in digest {
        let _ = write!(etag, "{byte:02x}");
    }
    etag.push('"');
    etag
}

fn models_snapshot_response(
    route: &'static str,
    started_at: Instant,
    headers: &HeaderMap,
    snapshot: &ModelsSnapshot,
) -> AxumResponse {
    if if_none_match_matches(headers, &snapshot.etag) {
        diagnostics::record_http_json_response(route, started_at.elapsed(), 0);
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response.headers_mut().insert(ETAG, snapshot.etag.clone());
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        return response;
    }

    let mut response = traced_json_bytes(route, started_at, snapshot.payload.clone());
    response.headers_mut().insert(ETAG, snapshot.etag.clone());
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn if_none_match_matches(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let Some(candidates) = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let etag = etag.to_str().unwrap_or_default();
    candidates.split(',').map(str::trim).any(|candidate| {
        candidate == "*"
            || candidate == etag
            || candidate
                .strip_prefix("W/")
                .is_some_and(|candidate| candidate == etag)
    })
}

fn model_discovery_refresh_candidates(
    config: &crate::config::AppConfig,
) -> Vec<crate::config::ProviderRuntimeConfig> {
    config
        .providers
        .values()
        .filter(|provider| {
            provider_supports_model_discovery(provider)
                && (provider.auth.kind == CredentialKind::None
                    || provider.has_configured_credential())
                && discovery_cache_needs_refresh(
                    provider,
                    &config.model_discovery_cache,
                    DEFAULT_DISCOVERY_CACHE_TTL,
                )
        })
        .cloned()
        .collect()
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SearchRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let query = request.query.trim().to_string();
    if query.is_empty() {
        return Err(bad_request("query must not be empty"));
    }
    let limit = request
        .limit
        .unwrap_or(SEARCH_DEFAULT_LIMIT)
        .clamp(1, SEARCH_MAX_LIMIT);
    let agent_ids = normalize_search_agent_ids(request.agent_ids)?;
    let source_kinds = normalize_search_types(&request.types);
    let search_result = state
        .host
        .search_memory_read_only(
            &query,
            limit,
            request.include_all_workspaces,
            &agent_ids,
            &source_kinds,
        )
        .await
        .map_err(error_response)?;
    traced_json(
        "/search",
        started_at,
        SearchResponse {
            query,
            limit,
            results: search_result.results,
            index_status: search_result.index_status,
            index_status_by_agent: search_result.index_status_by_agent,
        },
    )
}

pub async fn memory_get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<MemoryGetRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let started_at = std::time::Instant::now();
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let source_ref = request.source_ref.trim();
    if source_ref.is_empty() {
        return Err(bad_request("source_ref must not be empty"));
    }
    let memory = state
        .host
        .get_memory_read_only(source_ref, request.max_chars)
        .await
        .map_err(error_response)?
        .ok_or_else(|| not_found(format!("memory source {source_ref} not found")))?;
    traced_json("/memory/get", started_at, memory)
}

fn normalize_search_types(types: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for kind in types {
        let kind = kind.trim();
        if !kind.is_empty() && !normalized.iter().any(|existing| existing == kind) {
            normalized.push(kind.to_string());
        }
    }
    normalized
}

fn normalize_search_agent_ids(
    agent_ids: Option<Vec<String>>,
) -> Result<Vec<String>, (StatusCode, Json<Value>)> {
    let Some(agent_ids) = agent_ids else {
        return Ok(Vec::new());
    };
    let mut normalized = Vec::new();
    for agent_id in agent_ids {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            return Err(bad_request("agent_ids must not contain empty agent ids"));
        }
        if !normalized.iter().any(|existing| existing == agent_id) {
            normalized.push(agent_id.to_string());
        }
    }
    Ok(normalized)
}

/// Legacy control-plane capabilities plus the observer-sync capabilities
/// evaluated from durable verification results. Observer-sync capabilities
/// require their storage and consistency invariants to be verified for the
/// current database; a verification load failure degrades to the all-disabled
/// evaluator instead of failing the handshake.
fn handshake_capabilities(state: &AppState) -> Vec<&'static str> {
    let mut capabilities = vec![
        "agents.list",
        "agents.state",
        "agents.events",
        "agents.control",
        "auth.native-session.v1",
        "control.prompt-idempotency.v1",
        "control.prompt-attachments.v1",
        "brief.attachments.v1",
        "tui.remote",
    ];
    let verification = load_observer_sync_verification(state);
    capabilities.extend(advertised_observer_sync_capabilities(&verification));
    capabilities
}

/// Loads the durable observer-sync verification rows into the capability
/// evaluator input. A verification load failure degrades to the
/// all-disabled evaluator instead of failing the caller.
pub(crate) fn load_observer_sync_verification(
    state: &AppState,
) -> ObserverSyncCapabilityVerification {
    let _preflight = crate::diagnostics::attribution::CAPABILITY_PREFLIGHT.start();
    let mut verification = ObserverSyncCapabilityVerification::default();
    match state
        .host
        .runtime_db()
        .observer_sync_foundations_with_self_heal()
    {
        Ok(foundations) => {
            verification.runtime_identity_stable = foundations.runtime_identity_stable;
            verification.agent_identity_reserved = foundations.agent_identity_reserved;
            verification.roster_snapshot_verified = foundations.roster_snapshot_verified;
            verification.projection_snapshot_verified = foundations.projection_snapshot_verified;
            verification.event_projection_effect_complete =
                foundations.event_projection_effect_complete;
            verification.brief_atomic_linkage_verified = foundations.brief_atomic_linkage_verified;
            verification.conversation_read_verified = foundations.conversation_read_verified;
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "observer-sync foundation verification unavailable; keeping capabilities disabled"
            );
        }
    }
    verification
}

pub(crate) async fn load_observer_sync_verification_scoped(
    state: &AppState,
    deadline: tokio::time::Instant,
) -> Result<ObserverSyncCapabilityVerification, ProjectionFailure> {
    let db = state.host.runtime_db().clone();
    state
        .projection_gate
        .preflight(deadline, move |scope| {
            let _preflight = crate::diagnostics::attribution::CAPABILITY_PREFLIGHT.start();
            let db = db.with_read_scope(scope);
            db.check_read_scope()
                .map_err(error_response)
                .map_err(ProjectionFailure::from)?;
            // Self-heal's writer transaction is not interrupted by read cancellation.
            let foundations = db
                .observer_sync_foundations_with_self_heal()
                .map_err(error_response)
                .map_err(ProjectionFailure::from)?;
            db.check_read_scope()
                .map_err(error_response)
                .map_err(ProjectionFailure::from)?;
            Ok(ObserverSyncCapabilityVerification {
                runtime_identity_stable: foundations.runtime_identity_stable,
                agent_identity_reserved: foundations.agent_identity_reserved,
                roster_snapshot_verified: foundations.roster_snapshot_verified,
                projection_snapshot_verified: foundations.projection_snapshot_verified,
                event_projection_effect_complete: foundations.event_projection_effect_complete,
                brief_atomic_linkage_verified: foundations.brief_atomic_linkage_verified,
                conversation_read_verified: foundations.conversation_read_verified,
            })
        })
        .await
}

pub async fn handshake(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(authentication_error_response)?;
    let config = state.host.config();
    Ok(Json(HandshakeResponse {
        ok: true,
        protocol: HandshakeProtocol {
            name: "holon-control".to_string(),
            version: 1,
        },
        auth: HandshakeAuth {
            mode: if state.require_control_token {
                "bearer".to_string()
            } else {
                "local".to_string()
            },
            required: state.require_control_token,
        },
        capabilities: handshake_capabilities(&state)
            .into_iter()
            .map(str::to_string)
            .collect(),
        runtime: HandshakeRuntime {
            default_agent: config.default_agent_id.clone(),
            workspace_dir: config.workspace_dir.to_string_lossy().into_owned(),
            home_dir: config.home_dir.to_string_lossy().into_owned(),
            listen: config.http_addr.to_string(),
            advertise_url: state.advertise_url.clone(),
        },
        limits: Some(HandshakeLimits {
            prompt_body_max_bytes: config.stored_config.api.prompt.body_max_bytes() as u64,
            prompt_image_attachment_max_bytes: config
                .stored_config
                .api
                .prompt
                .attachment_max_bytes(),
            prompt_file_attachment_max_bytes: config
                .stored_config
                .api
                .prompt
                .attachment_max_bytes(),
        }),
    }))
}

pub async fn list_agent_entries(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AgentListQuery>,
) -> AxumResponse {
    let started_at = std::time::Instant::now();
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return authentication_error_response(error).into_response();
    }
    let budget = super::projection_gate::DEFAULT_BUILD_BUDGET;
    let deadline = tokio::time::Instant::now() + budget;
    // The projection cache key must distinguish parent filters; otherwise a
    // cached list built for one query would be served for every other query.
    let host = state.host.clone();
    let result = state
        .projection_gate
        .run_owned(
            ProjectionKey::AgentsList(query.parent.clone()),
            deadline,
            budget,
            move |context| async move {
                let runtime = tokio::runtime::Handle::current();
                context
                    .blocking(move |scope| {
                        let db = host.runtime_db().with_read_scope(scope.clone());
                        // Keep nested storage reads on this registered worker;
                        // owner waits use the same build's cancellation/deadline.
                        crate::runtime_db::read_block::with_scope(scope, || {
                            let projection_started = std::time::Instant::now();
                            let agents =
                                runtime
                                    .block_on(host.list_agent_entries_for_parent_in(
                                        query.parent.as_deref(),
                                        &db,
                                    ))
                                    .map_err(error_response)
                                    .map_err(ProjectionFailure::from)?;
                            crate::diagnostics::record_projection_agents_list(
                                projection_started.elapsed(),
                            );
                            let bytes = agents
                                .into_json_bytes()
                                .map_err(error_response)
                                .map_err(ProjectionFailure::from)?;
                            db.check_read_scope()
                                .map_err(error_response)
                                .map_err(ProjectionFailure::from)?;
                            Ok(bytes)
                        })
                    })
                    .await?
            },
        )
        .await;
    match result {
        Ok(bytes) => traced_json_bytes("/agents/list", started_at, bytes),
        Err(error) => projection_gate_error_response("/agents/list", error),
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AgentListQuery {
    /// Exact direct parent filter. The endpoint still returns only the
    /// lightweight public roster; private supervised children are exposed via
    /// the caller's AgentSummary.active_children projection.
    pub parent: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::Request};
    use tower::ServiceExt;

    fn test_state(home: &tempfile::TempDir) -> AppState {
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let config =
            crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host = RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new("unused")),
        )
        .unwrap();
        AppState::for_tcp(host)
    }

    async fn models_request(
        state: AppState,
        if_none_match: Option<&str>,
    ) -> (StatusCode, HeaderMap, Bytes) {
        let mut request = Request::builder().method("GET").uri("/api/models");
        if let Some(value) = if_none_match {
            request = request.header(IF_NONE_MATCH, value);
        }
        let response = crate::http::router(state)
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, body)
    }

    #[tokio::test]
    async fn models_handler_serves_conditional_requests_from_one_snapshot() {
        let home = tempfile::tempdir().unwrap();
        let state = test_state(&home);
        let (status, headers, body) = models_request(state.clone(), None).await;
        assert_eq!(status, StatusCode::OK);
        let etag = headers.get(ETAG).cloned().expect("etag header");
        assert_eq!(
            headers
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
        let value: Value = serde_json::from_slice(&body).expect("models json");
        assert!(value.get("available_models").is_some());
        assert!(value.get("model_availability").is_some());

        let (status, not_modified_headers, not_modified_body) =
            models_request(state.clone(), Some(etag.to_str().unwrap())).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert_eq!(not_modified_headers.get(ETAG), Some(&etag));
        assert!(not_modified_body.is_empty());

        let weak = format!("W/{}", etag.to_str().unwrap());
        let (status, _, _) = models_request(state.clone(), Some(&weak)).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        let (status, _, _) = models_request(state.clone(), Some("*")).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);

        let (status, _, cached_body) = models_request(state, Some("\"not-current\"")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cached_body, body);
    }

    #[tokio::test]
    async fn models_cache_reuses_snapshot_until_ttl_expires() {
        let home = tempfile::tempdir().unwrap();
        let state = test_state(&home);
        let first = state.models_cache.snapshot(&state.host).await.unwrap();
        let second = state.models_cache.snapshot(&state.host).await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        let mut expired = ModelsResponseCache::new();
        expired.ttl = StdDuration::ZERO;
        let source = state.host.config();
        let snapshot = expired
            .replace(source.clone(), (*source).clone())
            .await
            .unwrap();
        assert!(expired.cached(&source, snapshot.discovery_cache).is_none());
        expired.ttl = MODELS_SNAPSHOT_TTL;
        let cached = expired
            .cached(&source, snapshot.discovery_cache)
            .expect("snapshot within ttl");
        assert!(Arc::ptr_eq(&cached, &snapshot));

        let other_home = tempfile::tempdir().unwrap();
        std::fs::write(
            other_home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let other = crate::config::AppConfig::load_with_home(Some(other_home.path().to_path_buf()))
            .unwrap();
        assert!(expired
            .cached(&Arc::new(other), snapshot.discovery_cache)
            .is_none());

        std::fs::write(home.path().join("model-discovery-cache.json"), "{}").unwrap();
        assert!(expired
            .cached(&source, discovery_cache_stamp(home.path()))
            .is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn models_cache_coalesces_concurrent_builds() {
        let home = tempfile::tempdir().unwrap();
        let state = test_state(&home);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = state.models_cache.clone();
            let host = state.host.clone();
            handles.push(tokio::spawn(
                async move { cache.snapshot(&host).await.unwrap() },
            ));
        }
        let mut snapshots = Vec::new();
        for handle in handles {
            snapshots.push(handle.await.unwrap());
        }
        assert!(snapshots
            .windows(2)
            .all(|pair| Arc::ptr_eq(&pair[0], &pair[1])));
        assert_eq!(state.models_cache.builds(), 1);
    }

    #[test]
    fn entity_tag_is_stable_and_if_none_match_accepts_validator_forms() {
        let first = entity_tag(b"models");
        assert_eq!(first, entity_tag(b"models"));
        assert_ne!(first, entity_tag(b"models-2"));
        let etag = HeaderValue::from_str(&first).unwrap();

        let mut headers = HeaderMap::new();
        assert!(!if_none_match_matches(&headers, &etag));
        headers.insert(IF_NONE_MATCH, HeaderValue::from_static("\"other\""));
        assert!(!if_none_match_matches(&headers, &etag));
        headers.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
        assert!(if_none_match_matches(&headers, &etag));
        headers.insert(IF_NONE_MATCH, etag.clone());
        assert!(if_none_match_matches(&headers, &etag));
        let weak = HeaderValue::from_str(&format!("W/{first}")).unwrap();
        headers.insert(IF_NONE_MATCH, weak);
        assert!(if_none_match_matches(&headers, &etag));
    }
}
