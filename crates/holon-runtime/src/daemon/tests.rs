use super::runtime::{effective_config_mismatch_summary, set_prepare_runtime_before_server_hook};
use super::{
    config_fingerprint, daemon_paths, daemon_start, daemon_status, prepare_runtime_before_server,
    runtime_activity_summary, DaemonLifecycleState, RuntimeActivityState, RuntimeConfigSurface,
    RuntimeControlAuthMode, RuntimeStartupSurface, RuntimeStatusResponse,
};
use crate::config::{provider_registry_for_tests, AppConfig, ProviderId};
use crate::{
    host::RuntimeHost,
    provider::StubProvider,
    runtime_db::RuntimeDb,
    storage::AppStorage,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, CommandTaskSpec, RuntimeFailurePhase,
        RuntimeFailureSummary,
    },
};
use chrono::Utc;
use holon_daemon::state::{
    latest_known_runtime_failure, DaemonFailureDiagnostics, DaemonFailureDiagnosticsScope,
    DaemonFailureDiagnosticsStatus,
};
use std::{
    fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tempfile::tempdir;

static PREPARE_HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct PrepareHookGuard;

impl Drop for PrepareHookGuard {
    fn drop(&mut self) {
        set_prepare_runtime_before_server_hook(None);
    }
}

fn set_prepare_hook(
    hook: impl Fn(&AppConfig) -> anyhow::Result<()> + Send + Sync + 'static,
) -> PrepareHookGuard {
    set_prepare_runtime_before_server_hook(Some(Box::new(hook)));
    PrepareHookGuard
}

fn test_config() -> AppConfig {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
        user_home_dir: None,
        home_dir: home.path().to_path_buf(),
        data_dir: home.path().to_path_buf(),
        socket_path: home.path().join("run").join("holon.sock"),
        workspace_dir: workspace.path().to_path_buf(),
        context_window_messages: 8,
        context_window_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens: 4096,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        control_token: Some("secret".into()),
        control_auth_mode: crate::config::ControlAuthMode::Auto,
        auth: Default::default(),
        api_cors: Default::default(),
        api_projection: Default::default(),
        local_apps: Default::default(),
        config_file_path: home.path().join("config.json"),
        stored_config: Default::default(),
        default_model: crate::config::ModelRouteRef::new(
            ProviderId::anthropic(),
            crate::config::ProviderEndpointId::default_endpoint(),
            "claude-sonnet-4-6",
        ),
        fallback_models: vec![],
        vision_model: None,
        image_generation_model: None,
        vision_candidate_models: Vec::new(),
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
        max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
        command_task_output_retention_bytes: 8 * 1024 * 1024,
        command_task_output_quota_bytes: 64 * 1024 * 1024,
        command_task_min_free_disk_bytes: 512 * 1024 * 1024,
        command_task_min_free_disk_percent: 5,
        disable_provider_fallback: false,
        tui_alternate_screen: crate::config::AltScreenMode::Auto,
        validated_model_overrides: std::collections::HashMap::new(),
        validated_unknown_model_fallback: None,
        model_discovery_cache: Default::default(),
        providers: provider_registry_for_tests(None, Some("dummy"), home.path().join(".codex")),
        web_config: crate::web::WebConfig::default(),
    }
}

#[test]
fn config_fingerprint_changes_when_effective_config_changes() {
    let mut left = test_config();
    let right = left.clone();
    left.http_addr = "127.0.0.1:9999".into();
    assert_ne!(
        config_fingerprint(&left).unwrap(),
        config_fingerprint(&right).unwrap()
    );
}

fn available_persisted_daemon_diagnostics() -> DaemonFailureDiagnostics {
    DaemonFailureDiagnostics {
        scope: DaemonFailureDiagnosticsScope::PersistedDaemonOnly,
        status: DaemonFailureDiagnosticsStatus::Available,
        reason: None,
    }
}

#[tokio::test]
async fn daemon_status_reports_persisted_daemon_scope_without_db_agent_aggregation() {
    let config = test_config();
    let runtime_db =
        RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())
            .unwrap();
    let host_storage =
        AppStorage::new_global(config.home_dir.join("host"), runtime_db.clone()).unwrap();
    let identity = AgentIdentityRecord::new(
        "public-agent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    host_storage.append_agent_identity(&identity).unwrap();
    let failure = RuntimeFailureSummary {
        occurred_at: Utc::now(),
        summary: "public runtime failed".into(),
        phase: RuntimeFailurePhase::RuntimeTurn,
        detail_hint: Some("runtime artifact".into()),
        failure_artifact: None,
    };
    let mut agent = AgentState::new("public-agent");
    agent.last_runtime_failure = Some(failure.clone());
    runtime_db
        .agent_states()
        .upsert_for_fixture(&agent)
        .unwrap();

    let status = daemon_status(&config).await.unwrap();
    assert_eq!(status.state, DaemonLifecycleState::Stopped);
    // Default status reports only daemon-persisted failures: the agent's
    // DB-recorded failure must not be aggregated, and the diagnostics object
    // declares the persisted_daemon_only scope explicitly.
    assert_eq!(status.last_failure, None);
    assert_eq!(
        status.failure_diagnostics,
        Some(available_persisted_daemon_diagnostics())
    );
    let encoded = serde_json::to_value(&status).unwrap();
    assert_eq!(
        encoded["failure_diagnostics"]["scope"],
        "persisted_daemon_only"
    );
    assert_eq!(encoded["failure_diagnostics"]["status"], "available");
    // The DB still records the failure, proving the fixture is live while the
    // default status path simply never aggregates it.
    assert_eq!(
        latest_known_runtime_failure(&config).unwrap(),
        Some(failure)
    );
}

#[tokio::test]
async fn daemon_status_creates_no_runtime_db_and_leaves_corrupt_db_untouched() {
    let config = test_config();
    let status = daemon_status(&config).await.unwrap();
    assert_eq!(status.state, DaemonLifecycleState::Stopped);
    assert!(!config.runtime_db_path().exists());
    assert!(!config.runtime_db_lock_path().exists());
    assert_eq!(
        status.failure_diagnostics,
        Some(available_persisted_daemon_diagnostics())
    );

    // A pre-existing corrupt DB must not be migrated, repaired, or rewritten.
    let db_path = config.runtime_db_path();
    let db_dir = db_path.parent().unwrap();
    fs::create_dir_all(db_dir).unwrap();
    let corrupt = b"not a sqlite database at all".to_vec();
    fs::write(&db_path, &corrupt).unwrap();
    let status = daemon_status(&config).await.unwrap();
    assert_eq!(status.state, DaemonLifecycleState::Stopped);
    assert_eq!(fs::read(config.runtime_db_path()).unwrap(), corrupt);
}

#[test]
fn effective_config_mismatch_summary_lists_actionable_differences() {
    let mut expected = test_config();
    expected.http_addr = "0.0.0.0:7878".into();
    expected.callback_base_url = "http://192.168.1.10:7878".into();
    expected.control_auth_mode = crate::config::ControlAuthMode::Required;

    let mut actual_surface = RuntimeConfigSurface::new(&expected);
    actual_surface.runtime_max_output_tokens = expected.runtime_max_output_tokens;
    let status = RuntimeStatusResponse {
        ok: true,
        healthy: true,
        pid: 42,
        home_dir: expected.home_dir.clone(),
        socket_path: expected.socket_path.clone(),
        http_addr: "127.0.0.1:7878".into(),
        started_at: Utc::now(),
        config_fingerprint: "actual".into(),
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        activity: None,
        tunnel: None,
        startup_surface: Some(RuntimeStartupSurface {
            user_home_dir: expected.user_home_dir.clone(),
            home_dir: expected.home_dir.clone(),
            socket_path: expected.socket_path.clone(),
            workspace_dir: expected.workspace_dir.clone(),
            default_agent_id: expected.default_agent_id.clone(),
            callback_base_url: "http://127.0.0.1:7878".into(),
            control_token_configured: false,
            control_auth_mode: RuntimeControlAuthMode::Auto,
        }),
        runtime_surface: Some(actual_surface),
        last_failure: None,
        runtime_db_protection: None,
    };

    let summary = effective_config_mismatch_summary(&expected, &status);
    assert!(summary.contains("http_addr expected=\"0.0.0.0:7878\" actual=\"127.0.0.1:7878\""));
    assert!(summary.contains(
        "callback_base_url expected=\"http://192.168.1.10:7878\" actual=\"http://127.0.0.1:7878\""
    ));
    assert!(summary.contains("control_auth_mode expected=\"Required\" actual=\"Auto\""));
    assert!(summary.contains("control_token_configured expected=\"true\" actual=\"false\""));
    assert!(!summary.contains("secret"));
}

#[test]
fn runtime_config_surface_reports_available_search_provider_capabilities() {
    let surface = RuntimeConfigSurface::new(&test_config());

    assert_eq!(
        surface.available_search_provider_kinds.len(),
        crate::web::WebProviderKind::ALL.len()
    );
    let brave = surface
        .available_search_provider_kinds
        .iter()
        .find(|provider| provider.kind == "brave")
        .expect("brave provider kind should be reported");
    assert_eq!(
        brave.capabilities.auth,
        crate::web::WebProviderAuthClass::ApiKey
    );
    assert_eq!(brave.capabilities.default_priority, 80);
    let google = surface
        .available_search_provider_kinds
        .iter()
        .find(|provider| provider.kind == "google")
        .expect("google provider kind should be reported");
    assert_eq!(
        google.capabilities.auth,
        crate::web::WebProviderAuthClass::ApiKey
    );
    assert!(google.capabilities.supports_region_or_language);
    assert_eq!(google.capabilities.default_priority, 82);

    let native = surface
        .available_search_provider_kinds
        .iter()
        .find(|provider| provider.kind == "open_ai_native")
        .expect("native provider kind should be reported");
    assert_eq!(
        native.capabilities.status,
        crate::web::WebProviderSupportStatus::NativeOnly
    );
}

#[test]
fn runtime_config_surface_reports_credential_free_providers_as_ready() {
    let mut config = test_config();
    let ollama_id = crate::config::ProviderId::parse("ollama").unwrap();
    config.providers.insert(
        ollama_id.clone(),
        crate::config::ProviderRuntimeConfig {
            headers: Default::default(),
            id: ollama_id.clone(),
            route_provider: ollama_id,
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::AnthropicMessages,
            base_url: "http://127.0.0.1:11434".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::None,
                kind: crate::config::CredentialKind::None,
                env: None,
                profile: None,
                external: None,
                client_id: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );

    let surface = RuntimeConfigSurface::new(&config);
    let ollama = surface
        .providers
        .iter()
        .find(|provider| provider.id == "ollama")
        .expect("ollama provider should be reported");
    assert_eq!(ollama.credential_kind, "none");
    assert!(!ollama.api_key_supported);
    assert!(ollama.credential_configured);
}

#[test]
fn runtime_config_surface_projects_provider_discovery_decoder() {
    let mut config = test_config();
    let magpie_id = crate::config::ProviderId::parse("magpie").unwrap();
    config.providers.insert(
        magpie_id.clone(),
        crate::config::ProviderRuntimeConfig {
            headers: Default::default(),
            id: magpie_id.clone(),
            route_provider: magpie_id.clone(),
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "http://magpie.internal:8080".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::None,
                kind: crate::config::CredentialKind::None,
                env: None,
                profile: None,
                external: None,
                client_id: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );
    config.stored_config.providers.insert(
        magpie_id,
        crate::config::ProviderConfigFile {
            transport: crate::config::ProviderTransportKind::OpenAiChatCompletions,
            base_url: "http://magpie.internal:8080".into(),
            discovery: Some(crate::config::ProviderDiscoveryConfig {
                kind: crate::config::ProviderDiscoveryKind::OpenAiModels,
                path: Some("/v1/models".into()),
                decoder: Some(crate::config::ModelDiscoveryDecoder::Magpie),
            }),
            ..Default::default()
        },
    );

    let surface = RuntimeConfigSurface::new(&config);
    let magpie = surface
        .providers
        .iter()
        .find(|provider| provider.id == "magpie")
        .expect("magpie provider should be reported");
    let discovery = magpie
        .discovery
        .as_ref()
        .expect("configured discovery should be projected");
    assert_eq!(discovery.kind, "openai_models");
    assert_eq!(discovery.path.as_deref(), Some("/v1/models"));
    assert_eq!(discovery.decoder.as_deref(), Some("magpie"));
}

#[test]
fn runtime_config_surface_reports_decision_provider_settings() {
    let mut config = test_config();
    config.stored_config.decision.enabled = Some(true);
    config.stored_config.decision.model = Some("typesafe@default/jev-latest".into());

    let surface = RuntimeConfigSurface::new(&config);

    assert!(surface.decision.enabled);
    assert_eq!(
        surface.decision.model.as_deref(),
        Some("typesafe@default/jev-latest")
    );
    assert_eq!(surface.decision.provider.as_deref(), Some("typesafe"));
    assert_eq!(surface.decision.endpoint.as_deref(), Some("default"));
    assert_eq!(surface.decision.route_provider.as_deref(), Some("typesafe"));
    assert_eq!(
        surface.decision.transport.as_deref(),
        Some("ai_evaluation_model")
    );
    assert!(surface.decision.decision_capable);
    assert!(!surface.decision.credential_configured);
    assert!(!surface.decision.available);
    assert_eq!(
        surface.decision.unavailable_reason.as_deref(),
        Some("credential_missing")
    );
    assert_eq!(surface.decision.protocol.as_deref(), Some("jev"));
}

#[test]
fn runtime_config_surface_defaults_decision_provider_to_disabled() {
    let surface = RuntimeConfigSurface::new(&test_config());

    assert!(!surface.decision.enabled);
    assert_eq!(surface.decision.model, None);
    assert!(!surface.decision.local_onnx.enabled);
}

#[tokio::test]
async fn runtime_activity_summary_reports_idle_runtime() {
    let config = test_config();
    fs::create_dir_all(&config.workspace_dir).unwrap();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("ok"))).unwrap();
    let _runtime = host.default_runtime().await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let summary = runtime_activity_summary(&host).await.unwrap();
        if summary.state == RuntimeActivityState::Idle {
            assert_eq!(summary.active_agent_count, 1);
            assert_eq!(summary.active_task_count, 0);
            assert_eq!(summary.processing_agent_count, 0);
            assert_eq!(summary.waiting_agent_count, 0);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn runtime_activity_summary_reports_waiting_runtime() {
    let config = test_config();
    fs::create_dir_all(&config.workspace_dir).unwrap();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("ok"))).unwrap();
    let runtime = host.default_runtime().await.unwrap();
    let _task = runtime
        .schedule_command_task(
            "daemon wait".into(),
            CommandTaskSpec {
                cmd: "sleep 1".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            AuthorityClass::OperatorInstruction,
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let summary = runtime_activity_summary(&host).await.unwrap();
        if summary.state == RuntimeActivityState::Waiting && summary.active_task_count >= 1 {
            assert_eq!(summary.active_agent_count, 1);
            assert!(summary.waiting_agent_count >= 1);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[test]
fn daemon_start_runs_preparation_before_readiness_timeout_window() {
    let _guard = PREPARE_HOOK_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let config = test_config();
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = calls.clone();
    let _hook_guard = set_prepare_hook(move |_config| {
        hook_calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("test preparation domain failed")
    });

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let err = runtime
        .block_on(daemon_start(&config, &[], None))
        .unwrap_err()
        .to_string();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(err.contains("pre-server runtime preparation failed"));
    assert!(!err.contains("timed out waiting for runtime"));
    assert!(!daemon_paths(&config).log_path.exists());
}

#[test]
fn pre_server_preparation_failure_is_retryable() {
    let _guard = PREPARE_HOOK_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let config = test_config();
    let attempts = Arc::new(AtomicUsize::new(0));
    let hook_attempts = attempts.clone();
    let _hook_guard = set_prepare_hook(move |_config| {
        let attempt = hook_attempts.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            anyhow::bail!("test storage domain incomplete")
        }
        Ok(())
    });

    let first = prepare_runtime_before_server(&config)
        .unwrap_err()
        .to_string();
    let second = prepare_runtime_before_server(&config);

    assert!(first.contains("pre-server runtime preparation failed"));
    second.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}
