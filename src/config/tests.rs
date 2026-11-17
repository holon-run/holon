#[test]
fn runtime_db_readers_load_validation_and_restart_boundary() {
    let home = tempdir().unwrap();
    let path = persisted_config_path(home.path());
    let mut stored = HolonConfigFile::default();
    set_config_key(&mut stored, "model.default", "openai/gpt-5.4").unwrap();
    save_persisted_config_at(&path, &stored).unwrap();
    let initial = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    set_config_key(
        &mut stored,
        "runtime.db_readers.ordinary_max_connections",
        "8",
    )
    .unwrap();
    set_config_key(
        &mut stored,
        "runtime.db_readers.preparation_max_connections",
        "2",
    )
    .unwrap();
    save_persisted_config_at(&path, &stored).unwrap();
    let reloaded = initial.reload_runtime_config().unwrap();
    assert_eq!(
        reloaded.stored_config.runtime.db_readers,
        initial.stored_config.runtime.db_readers
    );
    let restarted = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    assert_eq!(
        restarted
            .stored_config
            .runtime
            .db_readers
            .resolve()
            .unwrap(),
        crate::runtime_db::RuntimeDbReaderConfig::new(8, 2).unwrap()
    );
    for (ordinary, preparation) in [(0, 1), (1, 0), (63, 2), (u32::MAX, u32::MAX)] {
        stored.runtime.db_readers.ordinary_max_connections = Some(ordinary);
        stored.runtime.db_readers.preparation_max_connections = Some(preparation);
        save_persisted_config_at(&path, &stored).unwrap();
        let error = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap_err();
        assert!(
            error.to_string().contains("runtime.db_readers"),
            "{error:#}"
        );
    }
}

#[test]
fn app_config_budget_defaults_match_context_and_catalog_defaults() {
    let mut env = EnvVarGuard::unset_many(&[
        "HOLON_PROMPT_BUDGET_ESTIMATED_TOKENS",
        "HOLON_COMPACTION_TRIGGER_ESTIMATED_TOKENS",
        "HOLON_COMPACTION_KEEP_RECENT_ESTIMATED_TOKENS",
    ]);
    env.set_var("HOLON_MODEL", "openai@default/gpt-5.4");
    let home = tempdir().unwrap();
    let config = AppConfig::load_with_home(Some(home.path().into())).unwrap();
    let base = ContextConfig::default();
    assert_eq!(
        config.prompt_budget_estimated_tokens,
        base.prompt_budget_estimated_tokens
    );
    assert_eq!(
        config.compaction_trigger_estimated_tokens,
        base.compaction_trigger_estimated_tokens,
    );
    assert_eq!(
        config.compaction_keep_recent_estimated_tokens,
        base.compaction_keep_recent_estimated_tokens,
    );
    let policy = RuntimeModelCatalog::from_config(&config).resolved_model_policy(
        &base,
        Some(&route_ref("openai@default/budget-test-unknown")),
    );
    assert_eq!(policy.context_window_tokens, None);
    assert_eq!(policy.prompt_budget_estimated_tokens, 128_000);
    assert_eq!(policy.compaction_trigger_estimated_tokens, 115_200);
    assert_eq!(policy.compaction_keep_recent_estimated_tokens, 43_776);
}

#[test]
fn app_config_budget_environment_is_explicit_snapshotted_and_not_persisted() {
    use crate::model_catalog::{ModelMetadataField, ModelMetadataOrigin};

    let mut env = EnvVarGuard::unset_many(&[
        "HOLON_COMPACTION_TRIGGER_ESTIMATED_TOKENS",
        "HOLON_COMPACTION_KEEP_RECENT_ESTIMATED_TOKENS",
    ]);
    env.set_var("HOLON_MODEL", "openai@default/gpt-5.4");
    env.set_var("HOLON_PROMPT_BUDGET_ESTIMATED_TOKENS", "128000");
    let home = tempdir().unwrap();
    let config = AppConfig::load_with_home(Some(home.path().into())).unwrap();
    assert_eq!(config.prompt_budget_estimated_tokens, 128_000);
    assert_eq!(config.compaction_trigger_estimated_tokens, 115_200);
    assert_eq!(config.compaction_keep_recent_estimated_tokens, 43_776);
    env.set_var("HOLON_PROMPT_BUDGET_ESTIMATED_TOKENS", "512");

    for config in [config.clone(), config.reload_runtime_config().unwrap()] {
        let catalog = RuntimeModelCatalog::from_config(&config);
        let policy = catalog.resolved_model_policy(&ContextConfig::default(), None);
        assert_eq!(policy.prompt_budget_estimated_tokens, 128_000);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 115_200);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 43_776);
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::PromptBudgetEstimatedTokens),
            Some(ModelMetadataOrigin::EnvironmentOverride),
        );
        assert_eq!(
            policy
                .evidence
                .source_for(ModelMetadataField::CompactionTriggerEstimatedTokens),
            Some(ModelMetadataOrigin::Derived),
        );
        let persisted = serde_json::to_value(&config.stored_config).unwrap();
        assert!(persisted
            .pointer("/runtime/context_budget_overrides")
            .is_none());
    }
}

#[test]
fn app_config_explicit_small_budget_derives_compression_and_preserves_overrides() {
    let mut env = EnvVarGuard::new();
    env.set_var("HOLON_MODEL", "openai@default/gpt-5.4");
    env.set_var("HOLON_PROMPT_BUDGET_ESTIMATED_TOKENS", "1000");
    env.set_var("HOLON_COMPACTION_TRIGGER_ESTIMATED_TOKENS", "700");
    env.set_var("HOLON_COMPACTION_KEEP_RECENT_ESTIMATED_TOKENS", "200");
    let home = tempdir().unwrap();
    let config = AppConfig::load_with_home(Some(home.path().into())).unwrap();
    let catalog = RuntimeModelCatalog::from_config(&config);
    for route in [
        "openai@default/gpt-5.4",
        "openai@default/budget-test-unknown",
        "opencode-go@default/iceman-contributor-free",
    ] {
        let policy =
            catalog.resolved_model_policy(&ContextConfig::default(), Some(&route_ref(route)));
        assert_eq!(policy.prompt_budget_estimated_tokens, 1_000);
        assert_eq!(policy.compaction_trigger_estimated_tokens, 700);
        assert_eq!(policy.compaction_keep_recent_estimated_tokens, 200);
    }
    env.unset_var("HOLON_COMPACTION_TRIGGER_ESTIMATED_TOKENS");
    env.unset_var("HOLON_COMPACTION_KEEP_RECENT_ESTIMATED_TOKENS");
    let config = AppConfig::load_with_home(Some(home.path().into())).unwrap();
    assert_eq!(config.compaction_trigger_estimated_tokens, 900);
    assert_eq!(config.compaction_keep_recent_estimated_tokens, 342);
}

#[test]
fn agent_timezone_configuration_validation_and_compatibility() {
    let mut state = crate::types::AgentState::new("timezone-test");
    let old_state = serde_json::to_value(&state).unwrap();
    assert!(old_state.get("timezone_override").is_none());
    assert!(
        serde_json::from_value::<crate::types::AgentState>(old_state)
            .unwrap()
            .timezone_override
            .is_none()
    );
    state.set_timezone(Some("America/Los_Angeles")).unwrap();
    assert!(state.set_timezone(Some("invalid/zone")).is_err());
    assert_eq!(
        state.timezone_override.as_deref(),
        Some("America/Los_Angeles")
    );
    let mut encoded = serde_json::to_value(&state).unwrap();
    assert_eq!(
        serde_json::from_value::<crate::types::AgentState>(encoded.clone())
            .unwrap()
            .timezone_override,
        state.timezone_override
    );
    encoded["timezone_override"] = serde_json::json!("invalid/zone");
    assert!(serde_json::from_value::<crate::types::AgentState>(encoded).is_err());
    state.set_timezone(None).unwrap();
    assert!(state.timezone_override.is_none());
}

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

use serde_json::{json, Value};
use tempfile::tempdir;

use crate::context::ContextConfig;
use crate::model_catalog::{
    BuiltInModelMetadata, ModelCapabilityOverride, ModelMetadataSource, ModelRuntimeOverride,
};
use crate::model_discovery::{ModelDiscoveryCacheFile, ProviderModelDiscoveryCache};
use crate::provider::ProviderNativeWebSearchKind;

use crate::config::{
    built_in_provider_registry_with_settings, credential_store_path, default_holon_home,
    get_config_key, get_config_value, load_persisted_config_at, persisted_config_path,
    provider_registry_for_tests, resolve_anthropic_context_management_config,
    save_persisted_config_at, set_config_key, set_credential_profile_at, unset_config_key,
    AnthropicCacheStrategy, AppConfig, ControlAuthMode, ControlTransportKind, CredentialKind,
    CredentialSource, CredentialStoreFile, HolonConfigFile, ModelConfigFile, ModelRef,
    ModelRouteCapability, ModelRouteRef, ModelsConfigFile, ProviderAuthConfig,
    ProviderBuiltinWebSearchConfig, ProviderConfigFile, ProviderEndpointConfigFile,
    ProviderEndpointId, ProviderId, ProviderPlanConfigFile, ProviderRuntimeConfig,
    ProviderTransportKind, RuntimeModelCatalog, XSearchRuntimeConfig, DEFAULT_LOCAL_AGENT_ID,
    DEFAULT_X_SEARCH_MODEL, OPENAI_CODEX_CREDENTIAL_PROFILE,
};

struct EnvVarSnapshot {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

struct EnvVarGuard {
    snapshots: Vec<EnvVarSnapshot>,
    _lock: MutexGuard<'static, ()>,
}

const VERCEL_AI_GATEWAY_ENV_KEYS: &[&str] = &[
    "VERCEL_OIDC_TOKEN",
    "AI_GATEWAY_API_KEY",
    "VERCEL_AI_GATEWAY_API_KEY",
];

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let mut guard = Self::new();
        guard.set_var(key, value);
        guard
    }

    fn unset(key: &'static str) -> Self {
        let mut guard = Self::new();
        guard.unset_var(key);
        guard
    }

    fn unset_many(keys: &[&'static str]) -> Self {
        let mut guard = Self::new();
        for key in keys {
            guard.unset_var(key);
        }
        guard
    }

    fn set_and_unset(
        set_vars: &[(&'static str, &std::ffi::OsStr)],
        unset_vars: &[&'static str],
    ) -> Self {
        let mut guard = Self::new();
        for (key, value) in set_vars {
            guard.set_var(key, value);
        }
        for key in unset_vars {
            guard.unset_var(key);
        }
        guard
    }

    fn new() -> Self {
        Self {
            snapshots: Vec::new(),
            _lock: crate::test_env::lock_env(),
        }
    }

    fn set_var(&mut self, key: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        self.snapshots.push(EnvVarSnapshot {
            key,
            original: std::env::var_os(key),
        });
        std::env::set_var(key, value);
    }

    fn unset_var(&mut self, key: &'static str) {
        self.snapshots.push(EnvVarSnapshot {
            key,
            original: std::env::var_os(key),
        });
        std::env::remove_var(key);
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for snapshot in self.snapshots.iter().rev() {
            if let Some(value) = &snapshot.original {
                std::env::set_var(snapshot.key, value);
            } else {
                std::env::remove_var(snapshot.key);
            }
        }
    }
}

struct TestAppConfigFixture {
    _home_dir: tempfile::TempDir,
    _workspace_dir: tempfile::TempDir,
    config: AppConfig,
}

fn route_ref(value: &str) -> ModelRouteRef {
    ModelRouteRef::parse_compatible(value).unwrap()
}

fn test_app_config(default_model: &str, fallback_models: &[&str]) -> TestAppConfigFixture {
    let home_dir = tempdir().unwrap();
    let workspace_dir = tempdir().unwrap();
    let home_path = home_dir.path().to_path_buf();
    let workspace_path = workspace_dir.path().to_path_buf();
    let config = AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
        user_home_dir: None,
        home_dir: home_path.clone(),
        data_dir: home_path.clone(),
        socket_path: home_path.join("run").join("holon.sock"),
        workspace_dir: workspace_path,
        context_window_messages: 8,
        context_window_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens: 4096,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        control_token: Some("control-value".into()),
        control_auth_mode: ControlAuthMode::Auto,
        auth: Default::default(),
        api_cors: Default::default(),
        api_projection: Default::default(),
        local_apps: Default::default(),
        config_file_path: home_path.join("config.json"),
        stored_config: Default::default(),
        default_model: route_ref(default_model),
        fallback_models: fallback_models
            .iter()
            .map(|value| route_ref(value))
            .collect(),
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
        validated_model_overrides: HashMap::new(),
        validated_unknown_model_fallback: None,
        model_discovery_cache: ModelDiscoveryCacheFile::default(),
        providers: provider_registry_for_tests(
            Some("openai-key"),
            Some("anthropic-token"),
            PathBuf::from("/tmp/codex-home"),
        ),
        web_config: crate::web::WebConfig::default(),
    };
    TestAppConfigFixture {
        _home_dir: home_dir,
        _workspace_dir: workspace_dir,
        config,
    }
}

#[test]
fn disabled_x_search_config_is_unavailable() {
    let mut fixture = test_app_config("openai/gpt-4o-mini", &[]);
    fixture.config.stored_config.x_search.enabled = Some(false);
    assert!(XSearchRuntimeConfig::from_app_config(&fixture.config)
        .unwrap()
        .is_none());
}

#[test]
fn x_search_auto_enables_with_xai_credential_and_supports_model_override() {
    let mut fixture = test_app_config("openai/gpt-4o-mini", &[]);
    let mut xai = fixture
        .config
        .providers
        .get(&ProviderId::openai())
        .unwrap()
        .clone();
    let xai_id = ProviderId::parse("xai").unwrap();
    xai.id = xai_id.clone();
    xai.route_provider = xai_id.clone();
    xai.base_url = "https://api.x.ai/v1".into();
    xai.credential = Some("xai-key".into());
    xai.builtin_web_search = None;
    fixture.config.providers.insert(xai_id, xai);

    let automatic = XSearchRuntimeConfig::from_app_config(&fixture.config)
        .unwrap()
        .unwrap();
    assert_eq!(automatic.model, DEFAULT_X_SEARCH_MODEL);

    fixture.config.stored_config.x_search.model = Some("xai/grok-4.5".into());
    let overridden = XSearchRuntimeConfig::from_app_config(&fixture.config)
        .unwrap()
        .unwrap();
    assert_eq!(overridden.model, "grok-4.5");
}

#[test]
fn app_config_defaults_unset_agent_env_to_main_agent() {
    let dir = tempdir().unwrap();
    let _agent_guard = EnvVarGuard::unset("HOLON_AGENT_ID");
    save_persisted_config_at(
        &persisted_config_path(dir.path()),
        &HolonConfigFile {
            model: ModelConfigFile {
                default: Some("openai/gpt-5.4".into()),
                ..ModelConfigFile::default()
            },
            ..HolonConfigFile::default()
        },
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();

    assert_eq!(config.default_agent_id, DEFAULT_LOCAL_AGENT_ID);
}

#[test]
fn app_config_honors_explicit_default_agent_env() {
    let dir = tempdir().unwrap();
    let _agent_guard = EnvVarGuard::set("HOLON_AGENT_ID", "release-bot");
    save_persisted_config_at(
        &persisted_config_path(dir.path()),
        &HolonConfigFile {
            model: ModelConfigFile {
                default: Some("openai/gpt-5.4".into()),
                ..ModelConfigFile::default()
            },
            ..HolonConfigFile::default()
        },
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();

    assert_eq!(config.default_agent_id, "release-bot");
}

#[test]
fn config_falls_back_to_settings_values() {
    let _base_url_guard = EnvVarGuard::unset("ANTHROPIC_BASE_URL");

    let mut settings = HashMap::new();
    settings.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        "https://example.com".to_string(),
    );

    let value = get_config_value("ANTHROPIC_BASE_URL", None, &settings);
    assert_eq!(value.as_deref(), Some("https://example.com"));
}

#[test]
fn app_config_load_materializes_openai_codex_profile_from_existing_store() {
    let dir = tempdir().unwrap();
    let _agent_guard = EnvVarGuard::unset("HOLON_AGENT_ID");
    save_persisted_config_at(
        &persisted_config_path(dir.path()),
        &HolonConfigFile {
            model: ModelConfigFile {
                default: Some("openai-codex/gpt-5.4".into()),
                ..ModelConfigFile::default()
            },
            ..HolonConfigFile::default()
        },
    )
    .unwrap();
    set_credential_profile_at(
            &credential_store_path(dir.path()),
            OPENAI_CODEX_CREDENTIAL_PROFILE,
            CredentialKind::OAuth,
            "{\"tokens\":{\"access_token\":\"token\",\"refresh_token\":\"refresh\",\"account_id\":\"acct\"}}"
                .into(),
        )
        .unwrap();

    let config = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();
    let openai_codex = config.providers.get(&ProviderId::openai_codex()).unwrap();

    assert_eq!(
        openai_codex.auth.profile.as_deref(),
        Some(OPENAI_CODEX_CREDENTIAL_PROFILE)
    );
    let credential: Value = serde_json::from_str(
        openai_codex
            .credential
            .as_deref()
            .expect("openai-codex credential material should be loaded"),
    )
    .unwrap();
    assert_eq!(
        credential["tokens"]["access_token"],
        Value::String("token".into())
    );
}

#[test]
fn app_config_applies_provider_overrides_before_materializing_openai_codex_profile() {
    let dir = tempdir().unwrap();
    let _agent_guard = EnvVarGuard::unset("HOLON_AGENT_ID");
    save_persisted_config_at(
        &persisted_config_path(dir.path()),
        &HolonConfigFile {
            model: ModelConfigFile {
                default: Some("openai-codex/gpt-5.4".into()),
                ..ModelConfigFile::default()
            },
            providers: BTreeMap::from([(
                ProviderId::openai_codex(),
                ProviderConfigFile {
                    headers: Default::default(),
                    cache_capabilities: None,
                    transport: ProviderTransportKind::OpenAiCodexResponses,
                    base_url: "https://chatgpt.com/backend-api/codex".into(),
                    auth: ProviderAuthConfig {
                        source: CredentialSource::ExternalCli,
                        kind: CredentialKind::SessionToken,
                        env: None,
                        profile: None,
                        external: Some("codex_cli".into()),
                        client_id: None,
                    },
                    reasoning_effort: None,
                    builtin_web_search: None,
                    endpoints: BTreeMap::new(),
                    plans: BTreeMap::new(),
                },
            )]),
            ..HolonConfigFile::default()
        },
    )
    .unwrap();
    set_credential_profile_at(
        &credential_store_path(dir.path()),
        OPENAI_CODEX_CREDENTIAL_PROFILE,
        CredentialKind::ApiKey,
        "wrong-kind-token".into(),
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();
    let openai_codex = config.providers.get(&ProviderId::openai_codex()).unwrap();

    assert_eq!(openai_codex.auth.source, CredentialSource::ExternalCli);
    assert_eq!(openai_codex.auth.kind, CredentialKind::SessionToken);
    assert_eq!(openai_codex.credential, None);
}

#[test]
fn control_token_auto_mode_trusts_only_unix_transport() {
    let mut fixture = test_app_config("openai/gpt-5.4", &[]);

    assert!(fixture
        .config
        .control_token_required(ControlTransportKind::Tcp));
    assert!(!fixture
        .config
        .control_token_required(ControlTransportKind::Unix));

    fixture.config.control_token = None;
    assert!(!fixture
        .config
        .control_token_required(ControlTransportKind::Tcp));

    fixture.config.http_addr = "0.0.0.0:7878".into();
    assert!(fixture
        .config
        .control_token_required(ControlTransportKind::Tcp));
}

#[test]
fn anthropic_runtime_cache_strategy_defaults_to_claude_code_prompt_cache() {
    let _env_guard =
        EnvVarGuard::unset_many(&["HOLON_ANTHROPIC_CACHE_STRATEGY", "HOLON_ANTHROPIC_BETAS"]);

    let config = resolve_anthropic_context_management_config().unwrap();

    assert_eq!(
        config.cache_strategy,
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
    assert_eq!(
        config.betas,
        vec![
            "claude-code-20250219".to_string(),
            "prompt-caching-scope-2026-01-05".to_string()
        ]
    );
}

#[test]
fn default_holon_home_uses_home_directory() {
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/holon-home-test");
    assert_eq!(
        default_holon_home(),
        Path::new("/tmp/holon-home-test/.holon")
    );
}

#[test]
fn app_config_keeps_user_home_separate_from_holon_home() {
    let _home_guard = EnvVarGuard::set("HOME", "/tmp/holon-user-home-test");
    let holon_home = tempfile::tempdir().unwrap();
    std::fs::write(
        holon_home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(holon_home.path().to_path_buf())).unwrap();

    assert_eq!(
        config.user_home_dir,
        Some(PathBuf::from("/tmp/holon-user-home-test"))
    );
    assert_eq!(config.home_dir, holon_home.path());
}

#[test]
fn app_config_rejects_legacy_scheduler_config() {
    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{
          "model":{"default":"openai/gpt-5.4"},
          "runtime":{"scheduler":"legacy"}
        }"#,
    )
    .unwrap();
    let error = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap_err();
    assert!(error.to_string().contains("v0.31.1"));
}

#[test]
fn app_config_rejects_invalid_scheduler_env() {
    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let _env = EnvVarGuard::set("HOLON_SCHEDULER", "shadow");

    let error = AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf()))
        .unwrap_err();
    assert!(error.to_string().contains("is obsolete"));
}

#[test]
fn app_config_rejects_bad_credential_store_permissions_when_store_exists() {
    let dir = tempdir().unwrap();
    save_persisted_config_at(
        &persisted_config_path(dir.path()),
        &HolonConfigFile {
            model: ModelConfigFile {
                default: Some("openai/gpt-5.4".into()),
                ..ModelConfigFile::default()
            },
            ..HolonConfigFile::default()
        },
    )
    .unwrap();
    let store_path = credential_store_path(dir.path());
    fs::write(&store_path, r#"{"profiles":{}}"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&store_path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[cfg(unix)]
    {
        let err = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap_err();
        assert!(err.to_string().contains("chmod 600"));
    }
    #[cfg(not(unix))]
    AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap();

    let mut config = load_persisted_config_at(&persisted_config_path(dir.path())).unwrap();
    config.providers.insert(
        ProviderId::parse("openrouter").unwrap(),
        ProviderConfigFile {
            headers: Default::default(),
            cache_capabilities: None,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://openrouter.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::ApiKey,
                env: None,
                profile: Some("openrouter:default".into()),
                external: None,
                client_id: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
    );
    save_persisted_config_at(&persisted_config_path(dir.path()), &config).unwrap();

    #[cfg(unix)]
    {
        let err = AppConfig::load_with_home(Some(dir.path().to_path_buf())).unwrap_err();
        assert!(err.to_string().contains("chmod 600"));
    }
}

#[test]
fn built_in_provider_registry_includes_compatible_provider_defaults() {
    let _env = EnvVarGuard::unset_many(VERCEL_AI_GATEWAY_ENV_KEYS);
    let mut settings_env = HashMap::new();
    settings_env.insert("OPENROUTER_API_KEY".to_string(), "settings-key".to_string());
    settings_env.insert(
        "HOLON_OPENROUTER_BASE_URL".to_string(),
        "https://openrouter.example/api/v3".to_string(),
    );
    settings_env.insert("DEEPSEEK_API_KEY".to_string(), "deepseek-key".to_string());
    settings_env.insert("XIAOMI_API_KEY".to_string(), "xiaomi-key".to_string());
    settings_env.insert(
        "XIAOMI_TOKEN_PLAN_API_KEY".to_string(),
        "xiaomi-token-plan-key".to_string(),
    );
    settings_env.insert("ZAI_API_KEY".to_string(), "zai-key".to_string());
    settings_env.insert("BIGMODEL_API_KEY".to_string(), "bigmodel-key".to_string());
    settings_env.insert("DASHSCOPE_API_KEY".to_string(), "dashscope-key".to_string());
    settings_env.insert(
        "DASHSCOPE_TOKEN_PLAN_API_KEY".to_string(),
        "dashscope-token-plan-key".to_string(),
    );
    settings_env.insert(
        "DASHSCOPE_CODING_PLAN_API_KEY".to_string(),
        "dashscope-coding-plan-key".to_string(),
    );
    settings_env.insert("NEARAI_API_KEY".to_string(), "nearai-key".to_string());
    settings_env.insert("GEMINI_API_KEY".to_string(), "gemini-key".to_string());
    settings_env.insert(
        "HOLON_GEMINI_BASE_URL".to_string(),
        "https://gemini.example/v1beta".to_string(),
    );
    settings_env.insert(
        "VOLCENGINE_API_KEY".to_string(),
        "volcengine-key".to_string(),
    );
    settings_env.insert(
        "HOLON_VOLCENGINE_AGENT_BASE_URL".to_string(),
        "https://ark.example/api/v3".to_string(),
    );

    let providers = super::built_in_provider_registry_with_settings(&settings_env).unwrap();

    let openrouter = providers
        .get(&ProviderId::parse("openrouter").unwrap())
        .unwrap();
    assert_eq!(
        openrouter.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );
    assert_eq!(openrouter.base_url, "https://openrouter.example/api/v3");
    assert_eq!(openrouter.auth.env.as_deref(), Some("OPENROUTER_API_KEY"));
    assert_eq!(openrouter.credential.as_deref(), Some("settings-key"));

    let gemini = providers.get(&ProviderId::gemini()).unwrap();
    assert_eq!(
        gemini.transport,
        ProviderTransportKind::GeminiGenerateContent
    );
    assert_eq!(gemini.base_url, "https://gemini.example/v1beta");
    assert_eq!(gemini.auth.env.as_deref(), Some("GEMINI_API_KEY"));
    assert_eq!(gemini.credential.as_deref(), Some("gemini-key"));

    assert!(providers.get(&ProviderId::parse("qwen").unwrap()).is_none());

    let dashscope = providers
        .get(&ProviderId::parse("dashscope").unwrap())
        .unwrap();
    assert_eq!(
        dashscope.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(
        dashscope.base_url,
        "https://dashscope.aliyuncs.com/apps/anthropic"
    );
    assert_eq!(dashscope.auth.env.as_deref(), Some("DASHSCOPE_API_KEY"));
    assert_eq!(dashscope.credential.as_deref(), Some("dashscope-key"));
    assert_eq!(
        dashscope.context_management.cache_strategy,
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );

    let dashscope_token_plan = providers
        .get(&ProviderId::parse("dashscope-token-plan").unwrap())
        .unwrap();
    assert_eq!(
        dashscope_token_plan.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(
        dashscope_token_plan.base_url,
        "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic"
    );
    assert_eq!(
        dashscope_token_plan.auth.env.as_deref(),
        Some("DASHSCOPE_TOKEN_PLAN_API_KEY")
    );
    assert_eq!(
        dashscope_token_plan.credential.as_deref(),
        Some("dashscope-token-plan-key")
    );

    let dashscope_coding_plan = providers
        .get(&ProviderId::parse("dashscope-coding-plan").unwrap())
        .unwrap();
    assert_eq!(
        dashscope_coding_plan.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(
        dashscope_coding_plan.base_url,
        "https://coding.dashscope.aliyuncs.com/apps/anthropic"
    );
    assert_eq!(
        dashscope_coding_plan.auth.env.as_deref(),
        Some("DASHSCOPE_CODING_PLAN_API_KEY")
    );
    assert_eq!(
        dashscope_coding_plan.credential.as_deref(),
        Some("dashscope-coding-plan-key")
    );

    let volcengine = providers
        .get(&ProviderId::parse("volcengine").unwrap())
        .unwrap();
    assert_eq!(volcengine.transport, ProviderTransportKind::OpenAiResponses);
    assert_eq!(
        volcengine.base_url,
        "https://ark.cn-beijing.volces.com/api/v3"
    );
    assert_eq!(volcengine.auth.env.as_deref(), Some("VOLCENGINE_API_KEY"));
    assert_eq!(volcengine.credential.as_deref(), Some("volcengine-key"));
    // Backward compat: the old image-specific key now targets the standard endpoint.
    let mut volcengine_image_openai_only_env = HashMap::new();
    volcengine_image_openai_only_env.insert(
        "VOLCENGINE_IMAGE_OPENAI_API_KEY".to_string(),
        "volcengine-image-key".to_string(),
    );
    let volcengine_image_openai_only_providers =
        super::built_in_provider_registry_with_settings(&volcengine_image_openai_only_env).unwrap();
    let volcengine = volcengine_image_openai_only_providers
        .get(&ProviderId::parse("volcengine").unwrap())
        .unwrap();
    assert_eq!(
        volcengine.auth.env.as_deref(),
        Some("VOLCENGINE_IMAGE_OPENAI_API_KEY")
    );
    assert_eq!(
        volcengine.credential.as_deref(),
        Some("volcengine-image-key")
    );

    let nearai = providers
        .get(&ProviderId::parse("nearai").unwrap())
        .unwrap();
    assert_eq!(
        nearai.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );
    assert_eq!(nearai.base_url, "https://cloud-api.near.ai/v1");
    assert_eq!(nearai.auth.env.as_deref(), Some("NEARAI_API_KEY"));
    assert_eq!(nearai.credential.as_deref(), Some("nearai-key"));

    let stepfun_plan = providers
        .get(&ProviderId::parse("stepfun-plan").unwrap())
        .unwrap();
    assert_eq!(
        stepfun_plan.base_url,
        "https://api.stepfun.com/step_plan/v1"
    );
    assert_eq!(
        stepfun_plan.auth.env.as_deref(),
        Some("STEPFUN_PLAN_API_KEY or STEPFUN_API_KEY")
    );
    assert_eq!(
        providers
            .get(&ProviderId::parse("stepfun").unwrap())
            .unwrap()
            .base_url,
        "https://api.stepfun.com/v1"
    );

    let deepseek = providers
        .get(&ProviderId::parse("deepseek").unwrap())
        .unwrap();
    assert_eq!(deepseek.transport, ProviderTransportKind::AnthropicMessages);
    assert_eq!(deepseek.base_url, "https://api.deepseek.com/anthropic");
    assert_eq!(deepseek.credential.as_deref(), Some("deepseek-key"));

    let xiaomi = providers
        .get(&ProviderId::parse("xiaomi").unwrap())
        .unwrap();
    assert_eq!(xiaomi.transport, ProviderTransportKind::OpenAiResponses);
    assert_eq!(xiaomi.base_url, "https://api.xiaomimimo.com/v1");
    assert_eq!(xiaomi.credential.as_deref(), Some("xiaomi-key"));

    let xiaomi_token_plan = providers
        .get(&ProviderId::parse("xiaomi-token-plan").unwrap())
        .unwrap();
    assert_eq!(
        xiaomi_token_plan.transport,
        ProviderTransportKind::OpenAiResponses
    );
    assert_eq!(
        xiaomi_token_plan.base_url,
        "https://token-plan-cn.xiaomimimo.com/v1"
    );
    assert_eq!(
        xiaomi_token_plan.credential.as_deref(),
        Some("xiaomi-token-plan-key")
    );

    let zai = providers.get(&ProviderId::parse("zai").unwrap()).unwrap();
    assert_eq!(zai.transport, ProviderTransportKind::AnthropicMessages);
    assert_eq!(zai.base_url, "https://api.z.ai/api/anthropic");
    assert_eq!(zai.credential.as_deref(), Some("zai-key"));

    let bigmodel = providers
        .get(&ProviderId::parse("bigmodel").unwrap())
        .unwrap();
    assert_eq!(bigmodel.transport, ProviderTransportKind::AnthropicMessages);
    assert_eq!(bigmodel.base_url, "https://open.bigmodel.cn/api/anthropic");
    assert_eq!(bigmodel.credential.as_deref(), Some("bigmodel-key"));

    let minimax = providers
        .get(&ProviderId::parse("minimax").unwrap())
        .unwrap();
    assert_eq!(minimax.transport, ProviderTransportKind::AnthropicMessages);

    let synthetic = providers
        .get(&ProviderId::parse("synthetic").unwrap())
        .unwrap();
    assert_eq!(
        synthetic.transport,
        ProviderTransportKind::AnthropicMessages
    );

    let tokenhub = providers
        .get(&ProviderId::parse("tencent-tokenhub").unwrap())
        .unwrap();
    assert_eq!(
        tokenhub.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );

    let vllm = providers.get(&ProviderId::parse("vllm").unwrap()).unwrap();
    assert_eq!(vllm.auth.source, CredentialSource::None);
    assert_eq!(vllm.credential, None);

    let vercel_ai_gateway = providers
        .get(&ProviderId::parse("vercel-ai-gateway").unwrap())
        .unwrap();
    assert_eq!(
        vercel_ai_gateway.auth.env.as_deref(),
        Some("VERCEL_OIDC_TOKEN or AI_GATEWAY_API_KEY or VERCEL_AI_GATEWAY_API_KEY")
    );
    assert_eq!(vercel_ai_gateway.auth.kind, CredentialKind::BearerToken);
    assert_eq!(vercel_ai_gateway.credential, None);

    for provider in [
        "deepseek",
        "zai",
        "bigmodel",
        "minimax",
        "synthetic",
        "vercel-ai-gateway",
    ] {
        let config = providers
            .get(&ProviderId::parse(provider).unwrap())
            .unwrap();
        assert_eq!(
            config.context_management.cache_strategy,
            AnthropicCacheStrategy::ClaudeCodePromptCache,
            "{provider} should use Claude Code-like prompt-cache lowering by default"
        );
        assert!(
            config.context_management.betas.is_empty(),
            "{provider} should not auto-inject Claude-specific betas"
        );
    }
}

#[test]
fn vercel_ai_gateway_falls_back_to_api_key_auth() {
    let _env = EnvVarGuard::unset_many(VERCEL_AI_GATEWAY_ENV_KEYS);
    let mut settings_env = HashMap::new();
    settings_env.insert("AI_GATEWAY_API_KEY".to_string(), "api-key".to_string());

    let providers = super::built_in_provider_registry_with_settings(&settings_env).unwrap();
    let vercel_ai_gateway = providers
        .get(&ProviderId::parse("vercel-ai-gateway").unwrap())
        .unwrap();

    assert_eq!(vercel_ai_gateway.auth.source, CredentialSource::Env);
    assert_eq!(vercel_ai_gateway.auth.kind, CredentialKind::ApiKey);
    assert_eq!(
        vercel_ai_gateway.auth.env.as_deref(),
        Some("AI_GATEWAY_API_KEY")
    );
    assert_eq!(vercel_ai_gateway.credential.as_deref(), Some("api-key"));
}

#[test]
fn config_inspection_loads_provider_state_without_resolved_model() {
    let home = tempdir().unwrap();
    let codex_home = home.path().join("codex-home");
    let _env_guard = EnvVarGuard::set_and_unset(
        &[
            ("HOME", home.path().as_os_str()),
            ("CODEX_HOME", codex_home.as_os_str()),
        ],
        &[
            "HOLON_MODEL",
            "HOLON_MODEL_FALLBACKS",
            "OPENAI_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ARCEE_API_KEY",
            "BYTEPLUS_API_KEY",
            "BYTEPLUS_CODING_API_KEY",
            "CHUTES_API_KEY",
            "DEEPSEEK_API_KEY",
            "FIREWORKS_API_KEY",
            "HUGGINGFACE_API_KEY",
            "HF_TOKEN",
            "KILOCODE_API_KEY",
            "LITELLM_API_KEY",
            "MISTRAL_API_KEY",
            "MOONSHOT_API_KEY",
            "NEARAI_API_KEY",
            "NVIDIA_API_KEY",
            "OPENCODE_GO_API_KEY",
            "OPENROUTER_API_KEY",
            "QIANFAN_API_KEY",
            "QWEN_API_KEY",
            "DASHSCOPE_API_KEY",
            "STEPFUN_API_KEY",
            "STEPFUN_PLAN_API_KEY",
            "SYNTHETIC_API_KEY",
            "TOKENHUB_API_KEY",
            "TOGETHER_API_KEY",
            "VENICE_API_KEY",
            "VOLCENGINE_API_KEY",
            "VOLCENGINE_IMAGE_OPENAI_API_KEY",
            "VOLCENGINE_AGENT_API_KEY",
            "VOLCENGINE_CODING_API_KEY",
            "ARK_API_KEY",
            "XIAOMI_API_KEY",
            "XIAOMI_TOKEN_PLAN_API_KEY",
            "XAI_API_KEY",
            "ZAI_API_KEY",
            "BIGMODEL_API_KEY",
            "MINIMAX_API_KEY",
            "AI_GATEWAY_API_KEY",
            "VERCEL_AI_GATEWAY_API_KEY",
            "HOLON_TEST_MISSING_CUSTOM_OPENAI_API_KEY",
        ],
    );
    let provider_id = ProviderId::parse("custom-openai").unwrap();
    save_persisted_config_at(
        &persisted_config_path(home.path()),
        &HolonConfigFile {
            providers: BTreeMap::from([(
                provider_id.clone(),
                ProviderConfigFile {
                    headers: Default::default(),
                    cache_capabilities: None,
                    transport: ProviderTransportKind::OpenAiChatCompletions,
                    base_url: "https://custom.example/v1".into(),
                    auth: ProviderAuthConfig {
                        source: CredentialSource::Env,
                        kind: CredentialKind::ApiKey,
                        env: Some("HOLON_TEST_MISSING_CUSTOM_OPENAI_API_KEY".into()),
                        profile: None,
                        external: None,
                        client_id: None,
                    },
                    reasoning_effort: None,
                    builtin_web_search: None,
                    endpoints: BTreeMap::new(),
                    plans: BTreeMap::new(),
                },
            )]),
            ..HolonConfigFile::default()
        },
    )
    .unwrap();

    let runtime_error = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap_err();
    assert!(runtime_error
        .to_string()
        .contains("no default model configured"));

    let config =
        AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf())).unwrap();
    let provider = config.providers.get(&provider_id).unwrap();
    let view = super::provider_config_view(&config, provider);

    assert_eq!(view.id, "custom-openai");
    assert!(view.configured_in_config);
    assert!(!view.credential_configured);
    assert_eq!(config.default_model.as_string(), "openai@default/unknown");
}

#[test]
fn provider_chain_refs_match_independent_owned_selection_matrix() {
    let fixture = test_app_config("openai@default/gpt-5.4", &[]);
    let mut catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let routes = [
        catalog.default_model.clone(),
        ModelRouteRef::parse("openai@alternate/gpt-5.4").unwrap(),
        ModelRouteRef::parse("anthropic@default/fallback-汉字🦀").unwrap(),
        ModelRouteRef::parse("openai@default/override").unwrap(),
    ];
    for length in 0..=5 {
        for mut code in 0..4usize.pow(length) {
            catalog.fallback_models = (0..length)
                .map(|_| {
                    let route = routes[code % 4].clone();
                    code /= 4;
                    route
                })
                .collect();
            for model_override in [None, Some(&routes[0]), Some(&routes[3])] {
                for disabled in [false, true] {
                    catalog.disable_provider_fallback = disabled;
                    let primary = model_override.unwrap_or(&catalog.default_model);
                    // Original owning algorithm, independent of the iterator.
                    let mut expected = vec![primary.clone()];
                    if !disabled {
                        for route in &catalog.fallback_models {
                            if !expected.contains(route) {
                                expected.push(route.clone());
                            }
                        }
                    }
                    assert_eq!(
                        catalog
                            .provider_chain_refs(model_override)
                            .cloned()
                            .collect::<Vec<_>>(),
                        expected
                    );
                    assert_eq!(catalog.provider_chain(model_override), expected);
                }
            }
        }
    }
}

#[test]
fn provider_chain_refs_borrow_primary_and_first_fallback_without_copying() {
    let fixture = test_app_config("openai@default/gpt-5.4", &[]);
    let mut catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let primary = catalog.default_model.clone();
    let fallback = ModelRouteRef::parse("openai@alternate/gpt-5.4").unwrap();
    let other = ModelRouteRef::parse("anthropic@default/other").unwrap();
    catalog.fallback_models = vec![
        primary.clone(),
        fallback.clone(),
        fallback,
        primary.clone(),
        other,
    ];
    {
        let mut chain = catalog.provider_chain_refs(None);
        assert!(std::ptr::eq(chain.next().unwrap(), &catalog.default_model));
        assert!(std::ptr::eq(
            chain.next().unwrap(),
            &catalog.fallback_models[1]
        ));
        // Dropping a partially consumed iterator owns no route material.
    }
    let mut chain = catalog.provider_chain_refs(Some(&primary));
    assert!(std::ptr::eq(chain.next().unwrap(), &primary));
    for index in [1, 4] {
        assert!(std::ptr::eq(
            chain.next().unwrap(),
            &catalog.fallback_models[index]
        ));
    }
    assert!(chain.next().is_none());
    assert!(chain.next().is_none());
    drop(chain);
    catalog.disable_provider_fallback = true;
    let mut chain = catalog.provider_chain_refs(None);
    assert!(std::ptr::eq(chain.next().unwrap(), &catalog.default_model));
    assert!(chain.next().is_none());
}

#[test]
fn provider_chain_preserves_effective_order_and_deduplicates() {
    let fixture = test_app_config(
        "openai/gpt-5.4",
        &[
            "openai/gpt-5.4",
            "anthropic/claude-sonnet-4-6",
            "openai-codex/gpt-5.4",
            "anthropic/claude-sonnet-4-6",
        ],
    );

    let chain = fixture
        .config
        .provider_chain()
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();
    assert_eq!(
        chain,
        vec![
            "openai@default/gpt-5.4",
            "anthropic@default/claude-sonnet-4-6",
            "openai-codex@default/gpt-5.4",
        ]
    );
}

#[test]
fn provider_chain_returns_only_effective_model_when_fallback_disabled() {
    let mut fixture = test_app_config(
        "openai@default/gpt-5.4",
        &[
            "anthropic@default/claude-sonnet-4-6",
            "openai-codex@default/gpt-5.4",
        ],
    );
    fixture
        .config
        .stored_config
        .runtime
        .disable_provider_fallback = Some(true);
    fixture.config.disable_provider_fallback = true;

    let chain = fixture
        .config
        .provider_chain()
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();

    assert_eq!(chain, vec!["openai@default/gpt-5.4"]);
}

#[test]
fn provider_chain_with_override_follows_fallbacks_without_runtime_default() {
    let fixture = test_app_config(
        "anthropic@default/claude-sonnet-4-6",
        &[
            "openai@default/gpt-5.4",
            "anthropic@default/claude-sonnet-4-6",
        ],
    );

    let override_model = route_ref("openai@default/gpt-5.4-mini");
    let chain = fixture
        .config
        .provider_chain_with_override(Some(&override_model))
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();

    assert_eq!(
        chain,
        vec![
            "openai@default/gpt-5.4-mini",
            "openai@default/gpt-5.4",
            "anthropic@default/claude-sonnet-4-6",
        ]
    );
}

#[test]
fn resolves_compatible_reasoning_effort_for_non_codex_route() {
    let fixture = test_app_config("anthropic@default/claude-sonnet-4-6", &[]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("anthropic@default/claude-sonnet-4-6"),
            ModelRouteCapability::Turn,
        )
        .unwrap();

    assert!(route.validate_reasoning_effort("xhigh").is_ok());
    assert!(route.validate_reasoning_effort("none").is_ok());
    assert!(route.validate_reasoning_effort("arbitrary").is_err());
}

#[test]
fn runtime_model_catalog_separates_turn_and_decision_capabilities_per_model() {
    let mut fixture = test_app_config("openai@default/turn-model", &[]);
    let turn_model = ModelRef::parse("openai/turn-model").unwrap();
    let decision_model = ModelRef::parse("openai/decision-model").unwrap();
    let turn_override = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            agent_turn: Some(true),
            decision: Some(false),
            ..ModelCapabilityOverride::default()
        }),
        ..ModelRuntimeOverride::default()
    };
    let decision_override = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            agent_turn: Some(false),
            decision: Some(true),
            ..ModelCapabilityOverride::default()
        }),
        decision_protocol: Some(crate::model_catalog::DecisionProtocol::Jev),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .config
        .validated_model_overrides
        .insert(turn_model, turn_override);
    fixture
        .config
        .validated_model_overrides
        .insert(decision_model, decision_override);

    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let context = ContextConfig::default();
    let turn_route = route_ref("openai@default/turn-model");
    let decision_route = route_ref("openai@default/decision-model");

    assert!(catalog
        .resolve_explicit_model_route(&context, &turn_route, ModelRouteCapability::Turn)
        .is_some());
    assert!(catalog
        .resolve_explicit_model_route(&context, &turn_route, ModelRouteCapability::Decision)
        .is_none());
    assert!(catalog
        .resolve_explicit_model_route(&context, &decision_route, ModelRouteCapability::Turn)
        .is_none());
    let decision = catalog
        .resolve_explicit_model_route(&context, &decision_route, ModelRouteCapability::Decision)
        .expect("Decision-capable model should resolve through the shared provider");
    assert_eq!(
        decision.policy.decision_protocol,
        Some(crate::model_catalog::DecisionProtocol::Jev)
    );
}

#[test]
fn provider_chain_for_turn_starts_at_pending_fallback_model() {
    let fixture = test_app_config(
        "openai@default/gpt-5.4",
        &[
            "anthropic@default/claude-sonnet-4-6",
            "openai-codex@default/gpt-5.3-codex-spark",
        ],
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let pending = route_ref("anthropic/claude-sonnet-4-6");

    let chain = catalog
        .provider_chain_for_turn(None, Some(&pending))
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();

    assert_eq!(
        chain,
        vec![
            "anthropic@default/claude-sonnet-4-6",
            "openai-codex@default/gpt-5.3-codex-spark",
        ]
    );
}

#[test]
fn provider_chain_with_override_returns_only_override_when_fallback_disabled() {
    let mut fixture = test_app_config(
        "anthropic@default/claude-sonnet-4-6",
        &["openai@default/gpt-5.4", "openai-codex@default/gpt-5.4"],
    );
    fixture
        .config
        .stored_config
        .runtime
        .disable_provider_fallback = Some(true);
    fixture.config.disable_provider_fallback = true;

    let override_model = route_ref("openai/gpt-5.4-mini");
    let chain = fixture
        .config
        .provider_chain_with_override(Some(&override_model))
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();

    assert_eq!(chain, vec!["openai@default/gpt-5.4-mini"]);
}

#[test]
fn local_apps_config_set_get_unset_and_persistence() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "model.default", "openai/gpt-5.4").unwrap();
    let policy = " default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; img-src https: ";
    set_config_key(&mut config, "local_apps.max_asset_bytes", "134217728").unwrap();
    set_config_key(&mut config, "local_apps.csp", policy).unwrap();
    assert_eq!(config.local_apps.max_asset_bytes(), 128 * 1024 * 1024);
    assert_eq!(
        get_config_key(&config, "local_apps.max_asset_bytes").unwrap(),
        json!(128 * 1024 * 1024)
    );
    assert_eq!(
        get_config_key(&config, "local_apps.csp").unwrap(),
        json!(policy)
    );
    assert_eq!(
        config
            .local_apps
            .csp_header_value()
            .unwrap()
            .unwrap()
            .to_str()
            .unwrap(),
        policy
    );
    let encoded = toml::to_string(&config).unwrap();
    assert!(encoded.contains("[local_apps]"));
    let restored: HolonConfigFile = toml::from_str(&encoded).unwrap();
    assert_eq!(restored.local_apps, config.local_apps);

    let home = tempfile::tempdir().unwrap();
    let path = persisted_config_path(home.path());
    save_persisted_config_at(&path, &config).unwrap();
    let restored = load_persisted_config_at(&path).unwrap();
    assert_eq!(restored.local_apps, config.local_apps);
    let runtime =
        AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf())).unwrap();
    assert_eq!(runtime.local_apps, config.local_apps);

    unset_config_key(&mut config, "local_apps.csp").unwrap();
    unset_config_key(&mut config, "local_apps.max_asset_bytes").unwrap();
    assert!(config.local_apps.is_empty());
    assert_eq!(config.local_apps.max_asset_bytes(), 64 * 1024 * 1024);
    assert!(config.local_apps.csp_header_value().unwrap().is_none());
    assert_eq!(
        get_config_key(&config, "local_apps.max_asset_bytes").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "local_apps.csp").unwrap(),
        Value::Null
    );
    save_persisted_config_at(&path, &config).unwrap();
    let reloaded = runtime.reload_runtime_config().unwrap();
    assert_eq!(reloaded.local_apps, config.local_apps);
    assert_eq!(runtime.local_apps.max_asset_bytes(), 128 * 1024 * 1024);
    assert_eq!(runtime.local_apps.csp.as_deref(), Some(policy));
}

#[test]
fn local_apps_config_runtime_load_rejects_invalid_persisted_values() {
    for (local_apps, key) in [
        (json!({"max_asset_bytes": 0}), "local_apps.max_asset_bytes"),
        (json!({"max_asset_bytes": -1}), "failed to parse"),
        (
            json!({"max_asset_bytes": "18446744073709551616"}),
            "failed to parse",
        ),
        (
            json!({"csp": "default-src 'self'\r\nX-Injected: true"}),
            "local_apps.csp",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            persisted_config_path(home.path()),
            json!({"local_apps": local_apps}).to_string(),
        )
        .unwrap();
        let error =
            AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf()))
                .unwrap_err();
        assert!(format!("{error:#}").contains(key), "{error:#}");
    }
}

#[test]
fn api_prompt_config_rejects_invalid_and_overflowing_limits() {
    for value in ["0", "-1", "1.5", "64m", "18446744073709551615"] {
        let mut config = HolonConfigFile::default();
        assert!(set_config_key(&mut config, "api.prompt.attachment_max_bytes", value).is_err());
        assert!(config.api.prompt.is_empty());
    }
    for value in [0, u64::MAX] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            json!({"api": {"prompt": {"attachment_max_bytes": value}}}).to_string(),
        )
        .unwrap();
        let error = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap_err();
        assert!(error
            .to_string()
            .contains("api.prompt.attachment_max_bytes"));
    }
}

#[test]
fn load_persisted_config_materializes_auth_settings() {
    let home = tempdir().unwrap();
    save_persisted_config_at(
        &persisted_config_path(home.path()),
        &HolonConfigFile {
            auth: crate::config::AuthConfigFile {
                mode: Some(crate::authentication::AuthenticationMode::Oidc),
                oidc: Some(crate::config::OidcConfigFile {
                    issuer_url: "https://issuer.example".into(),
                    client_id: "client".into(),
                    client_secret_env: None,
                    redirect_uri: Some("http://localhost/callback".into()),
                }),
                session: crate::config::SessionConfigFile {
                    absolute_ttl_seconds: Some(900),
                    idle_ttl_seconds: Some(300),
                },
            },
            model: ModelConfigFile {
                default: Some("anthropic/claude-sonnet-4-6".into()),
                ..ModelConfigFile::default()
            },
            ..HolonConfigFile::default()
        },
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    assert_eq!(
        config.auth.mode,
        crate::authentication::AuthenticationMode::Oidc
    );
    let oidc = config.auth.oidc.as_ref().expect("OIDC settings");
    assert_eq!(oidc.issuer_url, "https://issuer.example");
    assert_eq!(oidc.client_id, "client");
    assert_eq!(
        oidc.redirect_uri.as_deref(),
        Some("http://localhost/callback")
    );
    assert_eq!(config.auth.session.absolute_ttl_seconds, Some(900));
    assert_eq!(config.auth.session.idle_ttl_seconds, 300);
}

#[test]
fn load_persisted_config_rejects_invalid_auth_settings() {
    let oidc = || crate::config::OidcConfigFile {
        issuer_url: "https://issuer.example".into(),
        client_id: "client".into(),
        client_secret_env: None,
        redirect_uri: None,
    };
    let cases = vec![
        (
            "local mode with OIDC settings",
            crate::config::AuthConfigFile {
                mode: Some(crate::authentication::AuthenticationMode::Local),
                oidc: Some(oidc()),
                ..Default::default()
            },
        ),
        (
            "OIDC mode without provider settings",
            crate::config::AuthConfigFile {
                mode: Some(crate::authentication::AuthenticationMode::Oidc),
                ..Default::default()
            },
        ),
        (
            "idle session TTL greater than absolute TTL",
            crate::config::AuthConfigFile {
                session: crate::config::SessionConfigFile {
                    absolute_ttl_seconds: Some(60),
                    idle_ttl_seconds: Some(61),
                },
                ..Default::default()
            },
        ),
    ];

    for (name, auth) in cases {
        let home = tempdir().unwrap();
        save_persisted_config_at(
            &persisted_config_path(home.path()),
            &HolonConfigFile {
                auth,
                model: ModelConfigFile {
                    default: Some("anthropic/claude-sonnet-4-6".into()),
                    ..ModelConfigFile::default()
                },
                ..HolonConfigFile::default()
            },
        )
        .unwrap();

        assert!(
            AppConfig::load_with_home(Some(home.path().to_path_buf())).is_err(),
            "{name} should be rejected"
        );
    }
}

#[test]
fn load_persisted_config_normalizes_zero_absolute_session_ttl_to_unlimited() {
    let home = tempdir().unwrap();
    save_persisted_config_at(
        &persisted_config_path(home.path()),
        &HolonConfigFile {
            auth: crate::config::AuthConfigFile {
                session: crate::config::SessionConfigFile {
                    absolute_ttl_seconds: Some(0),
                    idle_ttl_seconds: Some(1),
                },
                ..Default::default()
            },
            model: ModelConfigFile {
                default: Some("anthropic/claude-sonnet-4-6".into()),
                ..ModelConfigFile::default()
            },
            ..HolonConfigFile::default()
        },
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    assert_eq!(config.auth.session.absolute_ttl_seconds, None);
    assert_eq!(config.auth.session.idle_ttl_seconds, 1);
}

#[test]
fn load_persisted_config_clamps_default_idle_to_explicit_absolute_ttl() {
    let home = tempdir().unwrap();
    save_persisted_config_at(
        &persisted_config_path(home.path()),
        &HolonConfigFile {
            auth: crate::config::AuthConfigFile {
                session: crate::config::SessionConfigFile {
                    absolute_ttl_seconds: Some(259_200),
                    idle_ttl_seconds: None,
                },
                ..Default::default()
            },
            model: ModelConfigFile {
                default: Some("anthropic/claude-sonnet-4-6".into()),
                ..ModelConfigFile::default()
            },
            ..HolonConfigFile::default()
        },
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    assert_eq!(config.auth.session.absolute_ttl_seconds, Some(259_200));
    // An omitted idle TTL stays clamped to the explicit absolute TTL so a
    // config persisted before the 7-day default keeps loading.
    assert_eq!(config.auth.session.idle_ttl_seconds, 259_200);
}

#[test]
fn load_persisted_config_uses_default_endpoint_override_for_runtime_provider() {
    let home = tempdir().unwrap();
    let provider_id = ProviderId::parse("custom-openai").unwrap();
    save_persisted_config_at(
        &persisted_config_path(home.path()),
        &HolonConfigFile {
            model: ModelConfigFile {
                default: Some("custom-openai/test-model".into()),
                ..ModelConfigFile::default()
            },
            models: ModelsConfigFile {
                catalog: BTreeMap::from([(
                    "custom-openai/test-model".to_string(),
                    ModelRuntimeOverride::default(),
                )]),
                ..ModelsConfigFile::default()
            },
            providers: BTreeMap::from([(
                provider_id.clone(),
                ProviderConfigFile {
                    headers: Default::default(),
                    cache_capabilities: None,
                    transport: ProviderTransportKind::OpenAiResponses,
                    base_url: "https://legacy.example/v1".into(),
                    auth: ProviderAuthConfig::default(),
                    reasoning_effort: None,
                    builtin_web_search: None,
                    endpoints: BTreeMap::from([(
                        ProviderEndpointId::default_endpoint(),
                        ProviderEndpointConfigFile {
                            headers: Default::default(),
                            transport: Some(ProviderTransportKind::OpenAiChatCompletions),
                            base_url: Some("https://endpoint.example/v1".into()),
                            auth: Some(ProviderAuthConfig {
                                source: CredentialSource::None,
                                kind: CredentialKind::None,
                                env: None,
                                profile: None,
                                external: None,
                                client_id: None,
                            }),
                        },
                    )]),
                    plans: BTreeMap::new(),
                },
            )]),
            ..HolonConfigFile::default()
        },
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let provider = config.providers.get(&provider_id).unwrap();
    assert_eq!(
        provider.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );
    assert_eq!(provider.base_url, "https://endpoint.example/v1");
}

#[test]
fn load_persisted_config_materializes_plan_endpoint_alias() {
    let home = tempdir().unwrap();
    let provider_id = ProviderId::parse("custom-openai").unwrap();
    let endpoint_id = ProviderEndpointId::parse("coding").unwrap();
    save_persisted_config_at(
        &persisted_config_path(home.path()),
        &HolonConfigFile {
            model: ModelConfigFile {
                default: Some("custom-openai-coding/test-model".into()),
                ..ModelConfigFile::default()
            },
            providers: BTreeMap::from([(
                provider_id.clone(),
                ProviderConfigFile {
                    headers: Default::default(),
                    cache_capabilities: None,
                    transport: ProviderTransportKind::OpenAiResponses,
                    base_url: "https://default.example/v1".into(),
                    auth: ProviderAuthConfig::default(),
                    reasoning_effort: None,
                    builtin_web_search: None,
                    endpoints: BTreeMap::from([(
                        endpoint_id.clone(),
                        ProviderEndpointConfigFile {
                            headers: Default::default(),
                            transport: Some(ProviderTransportKind::OpenAiChatCompletions),
                            base_url: Some("https://coding.example/v1".into()),
                            auth: Some(ProviderAuthConfig {
                                source: CredentialSource::None,
                                kind: CredentialKind::None,
                                env: None,
                                profile: None,
                                external: None,
                                client_id: None,
                            }),
                        },
                    )]),
                    plans: BTreeMap::from([(
                        "coding".to_string(),
                        ProviderPlanConfigFile {
                            endpoint: Some(endpoint_id),
                        },
                    )]),
                },
            )]),
            ..HolonConfigFile::default()
        },
    )
    .unwrap();

    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let alias = ProviderId::parse("custom-openai-coding").unwrap();
    let provider = config.providers.get(&alias).unwrap();

    assert_eq!(provider.id, alias);
    assert_eq!(provider.route_provider, provider_id);
    assert_eq!(provider.route_endpoint.as_str(), "coding");
    assert_eq!(
        provider.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );
    assert_eq!(provider.base_url, "https://coding.example/v1");
}

#[test]
fn runtime_model_catalog_resolves_config_override_and_unknown_fallback() {
    let mut fixture = test_app_config("anthropic/claude-sonnet-4-6", &[]);
    let known_override = ModelRuntimeOverride {
        prompt_budget_estimated_tokens: Some(24_000),
        runtime_max_output_tokens: Some(4_096),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .config
        .stored_config
        .models
        .catalog
        .insert("anthropic/claude-sonnet-4-6".into(), known_override.clone());
    fixture.config.validated_model_overrides.insert(
        ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
        known_override,
    );
    let unknown_fallback = ModelRuntimeOverride {
        prompt_budget_estimated_tokens: Some(12_000),
        compaction_trigger_estimated_tokens: Some(10_000),
        ..ModelRuntimeOverride::default()
    };
    fixture.config.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
    fixture.config.validated_unknown_model_fallback = Some(unknown_fallback);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let base_context = ContextConfig {
        recent_messages: fixture.config.context_window_messages,
        recent_briefs: fixture.config.context_window_briefs,
        compaction_trigger_messages: fixture.config.compaction_trigger_messages,
        compaction_keep_recent_messages: fixture.config.compaction_keep_recent_messages,
        prompt_budget_estimated_tokens: fixture.config.prompt_budget_estimated_tokens,
        compaction_trigger_estimated_tokens: fixture.config.compaction_trigger_estimated_tokens,
        compaction_keep_recent_estimated_tokens: fixture
            .config
            .compaction_keep_recent_estimated_tokens,
        recent_episode_candidates: fixture.config.recent_episode_candidates,
        max_relevant_episodes: fixture.config.max_relevant_episodes,
        callback_base_url: "https://callback.example".into(),
        ..ContextConfig::default()
    };

    let known = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(known.prompt_budget_estimated_tokens, 24_000);
    assert_eq!(known.runtime_max_output_tokens, 4_096);
    let resolved_context = catalog.resolved_context_config(&base_context, None);
    assert_eq!(resolved_context.prompt_budget_estimated_tokens, 24_000);
    assert_eq!(
        resolved_context.callback_base_url,
        "https://callback.example"
    );

    let unknown =
        catalog.resolved_model_policy(&base_context, Some(&route_ref("openai/custom-model")));
    assert_eq!(unknown.prompt_budget_estimated_tokens, 12_000);
    assert_eq!(unknown.compaction_trigger_estimated_tokens, 10_000);
    assert_eq!(
        unknown.source,
        crate::model_catalog::ModelMetadataSource::UnknownFallback
    );
}

#[test]
fn view_image_vision_selection_uses_primary_when_image_capable() {
    let mut fixture = test_app_config("openai/gpt-5.4", &["anthropic/claude-sonnet-4-6"]);
    fixture.config.vision_candidate_models = vec![route_ref("openai/gpt-5.4")];
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(
        selection.selection_reason,
        "current_primary_model_supports_image_input"
    );
}

#[test]
fn view_image_vision_selection_uses_explicit_vision_model() {
    let mut fixture = test_app_config("arcee/trinity-mini", &["openai/gpt-5.4-mini"]);
    fixture.config.vision_model = Some(route_ref("openai/gpt-5.4"));
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("arcee"));
    assert_eq!(selection.primary_model.as_deref(), Some("trinity-mini"));
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(
        selection.selection_reason,
        "explicit_vision_model_supports_image_input"
    );
    assert_eq!(selection.candidates.len(), 1);
    assert!(selection.candidates[0].image_input);
}

#[test]
fn view_image_vision_selection_auto_discovers_authenticated_adapter() {
    let mut fixture = test_app_config("arcee/trinity-mini", &[]);
    fixture.config.vision_candidate_models = vec![route_ref("openai/gpt-5.4-mini")];
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("arcee"));
    assert_eq!(selection.primary_model.as_deref(), Some("trinity-mini"));
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(
        selection.selection_reason,
        "auto_discovered_vision_model_supports_image_input"
    );
    assert_eq!(selection.candidates.len(), 2);
    assert!(!selection.candidates[0].image_input);
    assert!(selection.candidates[1].image_input);
}

#[test]
fn view_image_vision_selection_uses_configured_custom_auto_discovery_capabilities() {
    let mut fixture = test_app_config("arcee/trinity-mini", &[]);
    let custom_model = ModelRef::parse("custom-openai/my-vision-model").unwrap();
    fixture.config.vision_candidate_models =
        vec![ModelRouteRef::from_legacy_model_ref(&custom_model)];
    fixture.config.providers.insert(
        custom_model.provider.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: custom_model.provider.clone(),
            route_provider: custom_model.provider.clone(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.example.com/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::None,
                kind: CredentialKind::None,
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
    let override_config = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            image_input: Some(true),
            ..ModelCapabilityOverride::default()
        }),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .config
        .validated_model_overrides
        .insert(custom_model.clone(), override_config.clone());
    fixture
        .config
        .stored_config
        .models
        .catalog
        .insert(custom_model.as_string(), override_config);
    let unknown_fallback = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            image_input: Some(false),
            ..ModelCapabilityOverride::default()
        }),
        ..ModelRuntimeOverride::default()
    };
    fixture.config.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
    fixture.config.validated_unknown_model_fallback = Some(unknown_fallback);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("custom-openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("my-vision-model"));
    assert_eq!(
        selection.selection_reason,
        "auto_discovered_vision_model_supports_image_input"
    );
    assert_eq!(selection.candidates.len(), 2);
    assert!(!selection.candidates[0].image_input);
    assert!(selection.candidates[1].image_input);
}

#[test]
fn view_image_vision_selection_uses_anthropic_messages_when_image_capable() {
    let mut fixture = test_app_config("anthropic/claude-sonnet-5", &[]);
    fixture.config.vision_candidate_models = vec![route_ref("anthropic/claude-sonnet-5")];
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("anthropic"));
    assert_eq!(selection.primary_model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(selection.vision_provider.as_deref(), Some("anthropic"));
    assert_eq!(selection.vision_model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(
        selection.selection_reason,
        "current_primary_model_supports_image_input"
    );
    assert_eq!(
        selection.candidates[0].reason,
        "model_advertises_image_input"
    );
}

#[test]
fn view_image_vision_selection_uses_native_gemini_when_image_capable() {
    let fixture = test_app_config("gemini/gemini-2.5-pro", &[]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("gemini"));
    assert_eq!(selection.vision_model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(
        selection.selection_reason,
        "current_primary_model_supports_image_input"
    );
}

#[test]
fn view_image_vision_selection_skips_image_capable_unsupported_transports() {
    let mut fixture = test_app_config("gemini/gemini-2.5-pro", &[]);
    fixture.config.vision_candidate_models = vec![route_ref("openai/gpt-5.4-mini")];
    fixture
        .config
        .providers
        .get_mut(&ProviderId::gemini())
        .unwrap()
        .transport = ProviderTransportKind::AiEvaluationModel;
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("gemini"));
    assert_eq!(selection.primary_model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(
        selection.selection_reason,
        "auto_discovered_vision_model_supports_image_input"
    );
    assert_eq!(selection.candidates.len(), 2);
    assert_eq!(
        selection.candidates[0].reason,
        "provider_transport_unsupported_for_view_image_observation"
    );
    assert_eq!(
        selection.candidates[1].reason,
        "model_advertises_image_input"
    );
}

#[test]
fn view_image_vision_selection_keeps_fallback_chain_as_compatibility_candidates() {
    let fixture = test_app_config("arcee/trinity-mini", &["openai/gpt-5.4-mini"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::VisionAdapter
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(
        selection.selection_reason,
        "auto_discovered_vision_model_supports_image_input"
    );
}

#[test]
fn view_image_vision_selection_reports_unavailable_without_image_capable_model() {
    let fixture = test_app_config("arcee/trinity-mini", &["arcee/trinity-large-preview"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::Unavailable
    );
    assert_eq!(
        selection.selection_reason,
        "no_configured_model_supports_view_image_observation"
    );
    assert!(selection.vision_provider.is_none());
    assert_eq!(selection.candidates.len(), 2);
    assert!(selection
        .candidates
        .iter()
        .all(|candidate| !candidate.image_input));
}

#[test]
fn view_image_vision_selection_prefers_primary_over_other_candidates() {
    // Primary (openai/gpt-5.4) supports image_input. vision_candidate_models contains
    // a different image-capable model (anthropic/claude-sonnet-4-6). The primary should
    // be selected first because it is tried before other candidates.
    let mut fixture = test_app_config("openai/gpt-5.4", &["anthropic/claude-sonnet-4-6"]);
    fixture.config.vision_candidate_models = vec![
        route_ref("anthropic/claude-sonnet-4-6"),
        route_ref("openai/gpt-5.4"),
    ];
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("openai"));
    assert_eq!(selection.vision_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(
        selection.selection_reason,
        "current_primary_model_supports_image_input"
    );
    // Primary appears first in candidates
    assert_eq!(selection.candidates[0].provider, "openai");
    assert_eq!(selection.candidates[0].model, "gpt-5.4");
}

#[test]
fn view_image_vision_selection_canonicalizes_and_deduplicates_model_aliases() {
    let mut fixture = test_app_config("arcee/trinity-mini", &[]);
    let mistral = ProviderId::parse("mistral").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture
        .config
        .providers
        .insert(mistral.clone(), built_ins.get(&mistral).unwrap().clone());
    fixture.config.vision_candidate_models = vec![
        ModelRouteRef::parse("mistral@default/pixtral-large-latest").unwrap(),
        ModelRouteRef::parse("mistral@default/mistral-medium-latest").unwrap(),
    ];
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection =
        catalog.select_view_image_vision_model_with_route(&ContextConfig::default(), None, None);

    assert_eq!(
        selection
            .selected_route
            .as_ref()
            .map(ModelRouteRef::as_string),
        Some("mistral@default/mistral-medium-latest".to_string())
    );
    assert_eq!(
        selection
            .selection
            .candidates
            .iter()
            .filter(|candidate| candidate.model_ref == "mistral@default/mistral-medium-latest")
            .count(),
        1
    );
    assert_eq!(selection.selection.candidates.len(), 2);
    assert!(selection
        .selection
        .candidates
        .iter()
        .all(|candidate| candidate.model_ref != "mistral@default/pixtral-large-latest"));
}

#[test]
fn runtime_model_catalog_materializes_legacy_provider_as_default_route_endpoint() {
    let fixture = test_app_config("openai/gpt-5.4", &[]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let route = catalog
        .resolve_model_route(
            &ContextConfig::default(),
            &ModelRef::parse("openai/gpt-5.4").unwrap(),
            ModelRouteCapability::Turn,
        )
        .unwrap();

    assert_eq!(route.model_ref.as_string(), "openai/gpt-5.4");
    assert_eq!(route.endpoint.provider.as_str(), "openai");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
    assert_eq!(
        route.endpoint.runtime_config.transport,
        ProviderTransportKind::OpenAiResponses
    );
}

#[test]
fn runtime_model_catalog_exposes_deepseek_responses_without_changing_default_route() {
    let mut fixture = test_app_config("deepseek/deepseek-v4-pro", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    let deepseek = ProviderId::parse("deepseek").unwrap();
    fixture
        .config
        .providers
        .insert(deepseek.clone(), built_ins.get(&deepseek).unwrap().clone());
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let default_route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("deepseek@default/deepseek-v4-pro"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(
        default_route.endpoint.runtime_config.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(
        default_route.endpoint.runtime_config.base_url,
        "https://api.deepseek.com/anthropic"
    );

    let responses_route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("deepseek@responses/deepseek-v4-pro"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(
        responses_route.route_ref.as_string(),
        "deepseek@responses/deepseek-v4-pro"
    );
    assert_eq!(
        responses_route.endpoint.runtime_config.transport,
        ProviderTransportKind::OpenAiResponses
    );
    assert_eq!(
        responses_route.endpoint.runtime_config.base_url,
        "https://api.deepseek.com/v1"
    );
    let search = responses_route
        .endpoint
        .runtime_config
        .builtin_web_search
        .as_ref()
        .unwrap();
    assert!(search.enabled);
    assert_eq!(search.kind, ProviderNativeWebSearchKind::DeepSeek);
    assert_eq!(search.advertised_tool_type, "web_search");
    assert_eq!(search.backend_kind, "deepseek_web_search");
    assert_eq!(
        default_route
            .provider_config()
            .builtin_web_search
            .as_ref()
            .unwrap()
            .advertised_tool_type,
        "web_search_20250305"
    );
    // The default-derived endpoint retains the search seed; model resolution
    // above translates it to the documented Responses tool contract.
    assert_eq!(
        catalog
            .provider_endpoints
            .iter()
            .find(|endpoint| endpoint.provider.as_str() == "deepseek"
                && endpoint.endpoint.as_str() == "responses")
            .unwrap()
            .runtime_config
            .builtin_web_search
            .as_ref()
            .unwrap()
            .advertised_tool_type,
        "web_search_20250305"
    );

    assert_eq!(
        ModelRouteRef::parse_compatible("deepseek/deepseek-v4-pro")
            .unwrap()
            .as_string(),
        "deepseek@default/deepseek-v4-pro"
    );
}

#[test]
fn runtime_model_catalog_deepseek_responses_search_is_model_specific() {
    let mut fixture = test_app_config("deepseek/deepseek-v4-pro", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    let deepseek = ProviderId::parse("deepseek").unwrap();
    let mut runtime = built_ins.get(&deepseek).unwrap().clone();
    runtime.credential = Some("test-deepseek-key".into());
    fixture.config.providers.insert(deepseek, runtime);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    for model in [
        "deepseek-v4-pro",
        "deepseek-flash",
        "deepseek-v4-flash",
        "deepseek-v3.2",
    ] {
        // Metadata resolution covers unknown/new model IDs without requiring catalog entries.
        let metadata = catalog
            .resolve_explicit_model_metadata(
                &ContextConfig::default(),
                &route_ref(&format!("deepseek@responses/{model}")),
            )
            .unwrap();
        let supports_search = model == "deepseek-v4-pro";
        assert_eq!(
            metadata
                .endpoint
                .runtime_config
                .builtin_web_search
                .is_some(),
            supports_search,
            "{model} must only advertise documented Responses search"
        );
        let provider = crate::provider::OpenAiProvider::from_runtime_config(
            &metadata.endpoint.runtime_config,
            model,
            1_024,
            &fixture.config.home_dir,
        )
        .unwrap();
        assert_eq!(
            crate::provider::AgentProvider::builtin_web_search(&provider).is_some(),
            supports_search,
            "{model} provider must preserve the model-specific search contract"
        );
    }
}

#[test]
fn runtime_model_catalog_deepseek_responses_search_respects_disabled_configuration() {
    let mut fixture = test_app_config("deepseek/deepseek-v4-pro", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    let deepseek = ProviderId::parse("deepseek").unwrap();
    for search in [
        None,
        Some(ProviderBuiltinWebSearchConfig {
            enabled: false,
            kind: ProviderNativeWebSearchKind::DeepSeek,
            advertised_tool_type: "web_search_20250305".into(),
            backend_kind: "deepseek_web_search".into(),
        }),
    ] {
        let mut runtime = built_ins.get(&deepseek).unwrap().clone();
        runtime.builtin_web_search = search;
        fixture.config.providers.insert(deepseek.clone(), runtime);
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);
        let route = catalog
            .resolve_explicit_model_route(
                &ContextConfig::default(),
                &route_ref("deepseek@responses/deepseek-v4-pro"),
                ModelRouteCapability::Turn,
            )
            .unwrap();
        assert!(route.provider_config().builtin_web_search.is_none());
    }
}

#[test]
fn runtime_model_catalog_deepseek_responses_search_respects_default_url_override() {
    let deepseek = ProviderId::parse("deepseek").unwrap();
    for (base_url, expected_responses_url, supports_search) in [
        (
            "https://api.deepseek.com/anthropic",
            "https://api.deepseek.com/v1",
            true,
        ),
        (
            "https://api.deepseek.com/v1/",
            "https://api.deepseek.com/v1",
            true,
        ),
        (
            "https://private.example/anthropic",
            "https://private.example/v1",
            false,
        ),
        (
            "https://private.example/v1",
            "https://private.example/v1",
            false,
        ),
        (
            "https://api.deepseek.com.private.example/anthropic",
            "https://api.deepseek.com.private.example/v1",
            false,
        ),
    ] {
        let mut fixture = test_app_config("deepseek/deepseek-v4-pro", &[]);
        let settings = HashMap::from([("HOLON_DEEPSEEK_BASE_URL".into(), base_url.into())]);
        let built_ins = built_in_provider_registry_with_settings(&settings).unwrap();
        let mut runtime = built_ins.get(&deepseek).unwrap().clone();
        assert_eq!(runtime.base_url, base_url);
        runtime.credential = Some("test-deepseek-key".into());
        fixture.config.providers.insert(deepseek.clone(), runtime);
        let catalog = RuntimeModelCatalog::from_config(&fixture.config);
        let route = catalog
            .resolve_explicit_model_route(
                &ContextConfig::default(),
                &route_ref("deepseek@responses/deepseek-v4-pro"),
                ModelRouteCapability::Turn,
            )
            .unwrap();
        assert_eq!(route.provider_config().base_url, expected_responses_url);
        assert_eq!(
            route.provider_config().builtin_web_search.is_some(),
            supports_search,
            "search must stay isolated for default URL {base_url}"
        );
        let provider = crate::provider::OpenAiProvider::from_runtime_config(
            route.provider_config(),
            "deepseek-v4-pro",
            1_024,
            &fixture.config.home_dir,
        )
        .unwrap();
        assert_eq!(
            crate::provider::AgentProvider::builtin_web_search(&provider).is_some(),
            supports_search,
            "provider must preserve search isolation for default URL {base_url}"
        );
    }
}

#[test]
fn runtime_model_catalog_deepseek_responses_search_respects_private_endpoint_override() {
    let mut fixture = test_app_config("deepseek/deepseek-v4-pro", &[]);
    let deepseek = ProviderId::parse("deepseek").unwrap();
    let responses = ProviderEndpointId::parse("responses").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    let runtime = built_ins.get(&deepseek).unwrap();
    let mut stored_config = HolonConfigFile::default();
    stored_config.providers.insert(
        deepseek.clone(),
        ProviderConfigFile {
            transport: runtime.transport,
            base_url: runtime.base_url.clone(),
            auth: runtime.auth.clone(),
            endpoints: BTreeMap::from([(
                responses.clone(),
                ProviderEndpointConfigFile {
                    transport: Some(ProviderTransportKind::OpenAiResponses),
                    base_url: Some("https://private.example/deepseek/v1".into()),
                    ..Default::default()
                },
            )]),
            plans: BTreeMap::from([(
                "private".into(),
                ProviderPlanConfigFile {
                    endpoint: Some(responses.clone()),
                },
            )]),
            ..Default::default()
        },
    );
    fixture.config.providers = super::resolve_provider_registry(
        &stored_config,
        &HashMap::new(),
        &CredentialStoreFile::default(),
    )
    .unwrap();
    assert!(
        fixture.config.providers[&deepseek]
            .builtin_web_search
            .as_ref()
            .unwrap()
            .enabled
    );
    let private = fixture
        .config
        .providers
        .get_mut(&ProviderId::parse("deepseek-private").unwrap())
        .unwrap();
    private.credential = Some("test-deepseek-key".into());
    assert_eq!(private.route_provider, deepseek);
    assert_eq!(private.route_endpoint, responses);
    assert!(private.builtin_web_search.is_none());

    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("deepseek@responses/deepseek-v4-pro"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(
        route.provider_config().base_url,
        "https://private.example/deepseek/v1"
    );
    assert!(route.provider_config().builtin_web_search.is_none());
    let provider = crate::provider::OpenAiProvider::from_runtime_config(
        route.provider_config(),
        "deepseek-v4-pro",
        1_024,
        &fixture.config.home_dir,
    )
    .unwrap();
    assert!(crate::provider::AgentProvider::builtin_web_search(&provider).is_none());
}

#[test]
fn runtime_model_catalog_derives_deepseek_responses_from_an_explicit_allowlist() {
    let mut fixture = test_app_config("deepseek/deepseek-v4-pro", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    let deepseek = ProviderId::parse("deepseek").unwrap();
    let mut runtime = built_ins.get(&deepseek).unwrap().clone();
    runtime.base_url = "https://proxy.example/deepseek/anthropic/".into();
    runtime.reasoning_effort = Some("high".into());
    runtime.codex_home = Some(PathBuf::from("/should-not-leak"));
    runtime.originator = Some("anthropic-only-originator".into());
    runtime.context_management.enabled = true;
    fixture.config.providers.insert(deepseek, runtime);

    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let responses_route = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("deepseek@responses/deepseek-v4-pro"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    let runtime = &responses_route.endpoint.runtime_config;

    assert_eq!(runtime.base_url, "https://proxy.example/deepseek/v1");
    assert_eq!(runtime.reasoning_effort.as_deref(), Some("high"));
    assert!(runtime.codex_home.is_none());
    assert!(runtime.originator.is_none());
    assert!(!runtime.context_management.enabled);
}

#[test]
fn runtime_model_catalog_opencode_go_standard_route_does_not_follow_discovery_protocol() {
    let mut fixture = test_app_config("opencode-go/kimi-k2.7-code", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    for legacy_provider in ["opencode-go", "opencode-go-messages"] {
        let provider = ProviderId::parse(legacy_provider).unwrap();
        fixture
            .config
            .providers
            .insert(provider.clone(), built_ins.get(&provider).unwrap().clone());
    }
    let mut catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let template = catalog
        .built_in_catalog
        .get(&ModelRef::parse("opencode-go/kimi-k2.7-code").unwrap())
        .unwrap()
        .clone();
    for (model, endpoint, transport) in [
        (
            "discovery-only-responses",
            "responses",
            ProviderTransportKind::OpenAiResponses,
        ),
        (
            "discovery-only-messages",
            "messages",
            ProviderTransportKind::AnthropicMessages,
        ),
        (
            "discovery-only-chat",
            "default",
            ProviderTransportKind::OpenAiChatCompletions,
        ),
    ] {
        let model_ref = ModelRef::parse(&format!("opencode-go/{model}")).unwrap();
        let mut metadata = template.clone();
        metadata.model_ref = model_ref.clone();
        metadata.endpoint = Some(ProviderEndpointId::parse(endpoint).unwrap());
        catalog
            .discovered_models
            .insert(model_ref.clone(), metadata);
        let expected_route = route_ref(&format!("opencode-go@messages/{model}"));
        let routes = catalog.available_model_routes();
        assert!(routes.contains(&expected_route));
        assert!(!routes.contains(&route_ref(&format!("opencode-go@default/{model}"))));
        let resolved = catalog
            .resolve_model_route(
                &ContextConfig::default(),
                &model_ref,
                ModelRouteCapability::Turn,
            )
            .unwrap();
        assert_eq!(resolved.route_ref, expected_route);
        assert_eq!(
            resolved.endpoint.runtime_config.transport,
            ProviderTransportKind::AnthropicMessages
        );
        let explicit = catalog.resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref(&format!("opencode-go@{endpoint}/{model}")),
            ModelRouteCapability::Turn,
        );
        if endpoint == "responses" {
            assert!(explicit.is_none());
        } else {
            assert_eq!(
                explicit.unwrap().endpoint.runtime_config.transport,
                transport
            );
        }
    }

    // Discovery must not replace an explicitly reviewed built-in route.
    let mut conflicting = template.clone();
    conflicting.endpoint = Some(ProviderEndpointId::parse("messages").unwrap());
    catalog
        .discovered_models
        .insert(template.model_ref.clone(), conflicting);
    let resolved = catalog
        .resolve_model_route(
            &ContextConfig::default(),
            &template.model_ref,
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(resolved.endpoint.endpoint.as_str(), "messages");
    assert!(catalog
        .available_model_routes()
        .contains(&route_ref("opencode-go@default/kimi-k2.7-code")));
}

#[test]
fn runtime_model_catalog_opencode_go_uses_messages_without_a_responses_endpoint() {
    let mut fixture = test_app_config("opencode-go/minimax-m3", &[]);
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    for legacy_provider in ["opencode-go", "opencode-go-messages"] {
        let provider = ProviderId::parse(legacy_provider).unwrap();
        fixture
            .config
            .providers
            .insert(provider.clone(), built_ins.get(&provider).unwrap().clone());
    }
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    assert!(catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("opencode-go@responses/minimax-m3"),
            ModelRouteCapability::Turn,
        )
        .is_none());

    let messages = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("opencode-go@messages/minimax-m3"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(messages.endpoint.provider.as_str(), "opencode-go");
    assert_eq!(messages.endpoint.endpoint.as_str(), "messages");
    assert_eq!(
        messages.endpoint.runtime_config.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(
        messages.endpoint.runtime_config.base_url,
        "https://opencode.ai/inference/go/anthropic/v1"
    );

    let chat = catalog
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route_ref("opencode-go@default/kimi-k2.7-code"),
            ModelRouteCapability::Turn,
        )
        .unwrap();
    assert_eq!(chat.endpoint.provider.as_str(), "opencode-go");
    assert_eq!(chat.endpoint.endpoint.as_str(), "default");
    assert_eq!(
        chat.endpoint.runtime_config.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );
    assert_eq!(
        chat.endpoint.runtime_config.base_url,
        "https://opencode.ai/inference/go/openai/v1"
    );

    for model in [
        "minimax-m3",
        "kimi-k2.7-code",
        "glm-5.3-flash",
        "future-model",
        "grok-4.7",
        "gpt-6-luna",
        "gpt-5.6-luna",
        "muse-spark-1.3-contributor",
        "muse-spark-1.2-contributor",
    ] {
        let resolved = catalog
            .resolve_model_route(
                &ContextConfig::default(),
                &ModelRef::parse(&format!("opencode-go/{model}")).unwrap(),
                ModelRouteCapability::Turn,
            )
            .unwrap();
        assert_eq!(resolved.endpoint.provider.as_str(), "opencode-go");
        assert_eq!(resolved.endpoint.endpoint.as_str(), "messages");
        assert_eq!(
            resolved.endpoint.runtime_config.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            resolved.endpoint.runtime_config.base_url,
            "https://opencode.ai/inference/go/anthropic/v1"
        );
    }
}

#[test]
fn runtime_model_catalog_resolves_image_generation_route() {
    let fixture = test_app_config("arcee/trinity-mini", &["openai/gpt-image-2"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let route = catalog
        .select_generate_image_route(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(route.model_ref.as_string(), "openai/gpt-image-2");
    assert_eq!(
        route.requested_capability,
        ModelRouteCapability::ImageGeneration
    );
    assert_eq!(route.endpoint.provider.as_str(), "openai");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
}

#[test]
fn runtime_model_catalog_resolves_xai_imagine_image_generation_route() {
    let mut fixture = test_app_config("openai/gpt-5.4", &[]);
    let xai = ProviderId::parse("xai").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture
        .config
        .providers
        .insert(xai.clone(), built_ins.get(&xai).unwrap().clone());
    fixture.config.image_generation_model = Some(route_ref("xai@default/grok-imagine-image-2.0"));
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let route = catalog
        .select_generate_image_route(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(route.model_ref.as_string(), "xai/grok-imagine-image-2.0");
    assert_eq!(
        route.requested_capability,
        ModelRouteCapability::ImageGeneration
    );
    assert_eq!(route.endpoint.provider.as_str(), "xai");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
}

#[test]
fn runtime_model_catalog_resolves_legacy_multi_endpoint_provider_identity() {
    let mut fixture = test_app_config("openai/gpt-5.4", &["volcengine/doubao-seedream-5.0-lite"]);
    let legacy_provider = ProviderId::parse("volcengine").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.config.providers.insert(
        legacy_provider.clone(),
        built_ins.get(&legacy_provider).unwrap().clone(),
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let route = catalog
        .select_generate_image_route(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(
        route.model_ref.as_string(),
        "volcengine/doubao-seedream-5.0-lite"
    );
    assert_eq!(
        route.requested_capability,
        ModelRouteCapability::ImageGeneration
    );
    assert_eq!(route.endpoint.provider.as_str(), "volcengine");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
    assert_eq!(route.endpoint.runtime_config.id.as_str(), "volcengine");
}

#[test]
fn generate_image_selection_auto_uses_turn_chain() {
    let fixture = test_app_config("arcee/trinity-mini", &["openai/gpt-image-2"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(selected.as_string(), "openai@default/gpt-image-2");
}

#[test]
fn generate_image_selection_uses_current_turn_fallback_model() {
    let mut fixture = test_app_config("openai/gpt-image-2", &["openai/recovery-image"]);
    let recovery_model = ModelRef::parse("openai/recovery-image").unwrap();
    let recovery_override = ModelRuntimeOverride {
        capabilities: Some(ModelCapabilityOverride {
            image_generation: Some(true),
            ..ModelCapabilityOverride::default()
        }),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .config
        .validated_model_overrides
        .insert(recovery_model.clone(), recovery_override.clone());
    fixture
        .config
        .stored_config
        .models
        .catalog
        .insert(recovery_model.as_string(), recovery_override);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selected = catalog
        .select_generate_image_model(
            &ContextConfig::default(),
            None,
            Some(&route_ref("openai/recovery-image")),
        )
        .unwrap();

    assert_eq!(selected.as_string(), "openai@default/recovery-image");
}

#[test]
fn generate_image_selection_uses_explicit_image_generation_model() {
    let mut fixture = test_app_config("openai@default/gpt-image-2", &[]);
    fixture.config.image_generation_model = Some(route_ref("openai-codex@default/gpt-6-sol"));
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(selected.as_string(), "openai-codex@default/gpt-6-sol");
}

#[test]
fn generate_image_selection_requires_explicit_model_capability() {
    let mut fixture = test_app_config("openai@default/gpt-image-2", &[]);
    fixture.config.image_generation_model = Some(route_ref("openai@default/gpt-5.4"));
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    assert!(catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .is_none());
}

#[test]
fn generate_image_selection_accepts_volcengine_image_openai_seedream() {
    let mut fixture = test_app_config("openai@default/gpt-5.4", &[]);
    fixture.config.image_generation_model = Some(route_ref(
        "volcengine-image-openai/doubao-seedream-5.0-lite",
    ));
    fixture.config.providers.insert(
        ProviderId::parse("volcengine-image-openai").unwrap(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: ProviderId::parse("volcengine-image-openai").unwrap(),
            route_provider: ProviderId::parse("volcengine").unwrap(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://ark.cn-beijing.volces.com/api/v3".into(),
            auth: ProviderAuthConfig::default(),
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(
        selected.as_string(),
        "volcengine@default/doubao-seedream-5.0-lite"
    );
}

#[test]
fn generate_image_selection_accepts_canonical_volcengine_seedream() {
    let mut fixture = test_app_config("openai@default/gpt-5.4", &[]);
    fixture.config.image_generation_model =
        Some(route_ref("volcengine@default/doubao-seedream-5.0-lite"));
    let legacy_provider = ProviderId::parse("volcengine").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.config.providers.insert(
        legacy_provider.clone(),
        built_ins.get(&legacy_provider).unwrap().clone(),
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(
        selected.as_string(),
        "volcengine@default/doubao-seedream-5.0-lite"
    );
}

#[test]
fn runtime_model_catalog_resolves_canonical_seedream_route_endpoint() {
    let mut fixture = test_app_config("openai@default/gpt-5.4", &[]);
    fixture.config.image_generation_model =
        Some(route_ref("volcengine@default/doubao-seedream-5.0-lite"));
    let legacy_provider = ProviderId::parse("volcengine").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.config.providers.insert(
        legacy_provider.clone(),
        built_ins.get(&legacy_provider).unwrap().clone(),
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let route = catalog
        .select_generate_image_route(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(
        route.model_ref.as_string(),
        "volcengine/doubao-seedream-5.0-lite"
    );
    assert_eq!(route.endpoint.provider.as_str(), "volcengine");
    assert_eq!(route.endpoint.endpoint.as_str(), "default");
    assert_eq!(route.endpoint.runtime_config.id.as_str(), "volcengine");
}

#[test]
fn runtime_model_catalog_uses_discovery_cache_between_overrides_and_builtins() {
    let mut fixture = test_app_config("openrouter/anthropic/claude-3.5-sonnet", &[]);
    let openrouter = ProviderId::parse("openrouter").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.config.providers.insert(
        openrouter.clone(),
        built_ins.get(&openrouter).unwrap().clone(),
    );
    let remote_model_ref = ModelRef::parse("openrouter/anthropic/claude-3.5-sonnet").unwrap();
    fixture.config.model_discovery_cache.providers.insert(
        openrouter.clone(),
        ProviderModelDiscoveryCache {
            provider: openrouter,
            fetched_at: chrono::Utc::now(),
            source_url: Some("https://openrouter.ai/api/v1/models".into()),
            response_hash: Some("sha256:test".into()),
            models: vec![BuiltInModelMetadata {
                model_ref: remote_model_ref.clone(),
                display_name: "Remote Claude".into(),
                description: "Remote discovered model.".into(),
                context_window_tokens: Some(123_456),
                effective_context_window_percent: 95,
                auto_compact_token_limit: None,
                default_max_output_tokens: Some(7_777),
                max_output_tokens_upper_limit: Some(7_777),
                default_verbosity: None,
                tool_output_truncation_estimated_tokens: None,
                capabilities: Default::default(),
                reasoning_effort_options: Vec::new(),
                source: ModelMetadataSource::RemoteDiscovered,
                endpoint: None,
            }],
        },
    );
    let unknown_fallback = ModelRuntimeOverride {
        prompt_budget_estimated_tokens: Some(12_000),
        ..ModelRuntimeOverride::default()
    };
    fixture.config.stored_config.model.unknown_fallback = Some(unknown_fallback.clone());
    fixture.config.validated_unknown_model_fallback = Some(unknown_fallback);

    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let base_context = ContextConfig {
        recent_messages: fixture.config.context_window_messages,
        recent_briefs: fixture.config.context_window_briefs,
        compaction_trigger_messages: fixture.config.compaction_trigger_messages,
        compaction_keep_recent_messages: fixture.config.compaction_keep_recent_messages,
        prompt_budget_estimated_tokens: fixture.config.prompt_budget_estimated_tokens,
        compaction_trigger_estimated_tokens: fixture.config.compaction_trigger_estimated_tokens,
        compaction_keep_recent_estimated_tokens: fixture
            .config
            .compaction_keep_recent_estimated_tokens,
        recent_episode_candidates: fixture.config.recent_episode_candidates,
        max_relevant_episodes: fixture.config.max_relevant_episodes,
        ..ContextConfig::default()
    };

    let remote = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(remote.source, ModelMetadataSource::RemoteDiscovered);
    assert_eq!(remote.prompt_budget_estimated_tokens, 117_283);
    assert_eq!(remote.runtime_max_output_tokens, 7_777);

    let available = catalog.available_models();
    let listed = available
        .iter()
        .find(|model| model.model_ref == remote_model_ref)
        .expect("remote model should be listed");
    assert_eq!(listed.source, ModelMetadataSource::RemoteDiscovered);
    assert_eq!(listed.display_name, remote.display_name);
    assert_eq!(listed.context_window_tokens, remote.context_window_tokens);
    assert_eq!(
        listed.default_max_output_tokens,
        Some(remote.runtime_max_output_tokens)
    );
    let route = catalog
        .resolve_explicit_model_metadata(
            &base_context,
            &route_ref("openrouter/anthropic/claude-3.5-sonnet"),
        )
        .expect("configured OpenRouter route should resolve");
    assert_eq!(route.policy, remote);

    let override_config = ModelRuntimeOverride {
        display_name: Some("Configured Claude".into()),
        prompt_budget_estimated_tokens: Some(42_000),
        ..ModelRuntimeOverride::default()
    };
    fixture
        .config
        .validated_model_overrides
        .insert(remote_model_ref.clone(), override_config.clone());
    fixture
        .config
        .stored_config
        .models
        .catalog
        .insert(remote_model_ref.as_string(), override_config);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let overridden = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(overridden.source, ModelMetadataSource::ConfigOverride);
    assert_eq!(overridden.display_name, "Configured Claude");
    assert_eq!(overridden.prompt_budget_estimated_tokens, 42_000);
    let listed = catalog
        .available_models()
        .into_iter()
        .find(|model| model.model_ref == remote_model_ref)
        .expect("overridden model should remain listed");
    assert_eq!(listed.display_name, overridden.display_name);
    assert_eq!(
        listed.context_window_tokens,
        overridden.context_window_tokens
    );
}

#[test]
fn runtime_model_catalog_resolves_codex_sol_to_builtin_budget() {
    let fixture = test_app_config("openai-codex/gpt-6-sol", &[]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let base_context = ContextConfig {
        recent_messages: fixture.config.context_window_messages,
        recent_briefs: fixture.config.context_window_briefs,
        compaction_trigger_messages: fixture.config.compaction_trigger_messages,
        compaction_keep_recent_messages: fixture.config.compaction_keep_recent_messages,
        prompt_budget_estimated_tokens: fixture.config.prompt_budget_estimated_tokens,
        compaction_trigger_estimated_tokens: fixture.config.compaction_trigger_estimated_tokens,
        compaction_keep_recent_estimated_tokens: fixture
            .config
            .compaction_keep_recent_estimated_tokens,
        recent_episode_candidates: fixture.config.recent_episode_candidates,
        max_relevant_episodes: fixture.config.max_relevant_episodes,
        ..ContextConfig::default()
    };

    let resolved = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(resolved.model_ref.as_string(), "openai-codex/gpt-6-sol");
    assert_eq!(resolved.prompt_budget_estimated_tokens, 258_400);
    assert_eq!(resolved.compaction_trigger_estimated_tokens, 232_560);
    assert_eq!(resolved.compaction_keep_recent_estimated_tokens, 88_372);
    assert_eq!(
        resolved.source,
        crate::model_catalog::ModelMetadataSource::BuiltInCatalog
    );
}

#[test]
fn default_provider_ready_with_configured_credential() {
    let fixture = test_app_config("openai/gpt-4o", &[]);
    assert!(
        fixture.config.default_provider_ready(),
        "openai with a configured credential should be ready"
    );
}

#[test]
fn default_provider_ready_matches_the_selected_endpoint() {
    let mut fixture = test_app_config("openai@coding/gpt-4o", &[]);
    let openai = ProviderId::openai();
    let default_provider = fixture.config.providers.get_mut(&openai).unwrap();
    default_provider.credential = None;

    let coding_alias = ProviderId::parse("openai-coding").unwrap();
    fixture.config.providers.insert(
        coding_alias.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: coding_alias,
            route_provider: openai,
            route_endpoint: ProviderEndpointId::parse("coding").unwrap(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://coding.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENAI_CODING_API_KEY".into()),
                profile: None,
                external: None,
                client_id: None,
            },
            credential: Some("coding-key".into()),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );

    assert!(
        fixture.config.default_provider_ready(),
        "the selected coding endpoint credential should determine readiness"
    );
}

#[test]
fn default_provider_ready_for_credential_source_none() {
    let mut fixture = test_app_config("openai/gpt-4o", &[]);
    // Simulate a local provider (e.g. vllm) with CredentialSource::None.
    let vllm = ProviderId::parse("vllm").unwrap();
    fixture.config.providers.insert(
        vllm.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: vllm.clone(),
            route_provider: vllm.clone(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiChatCompletions,
            base_url: "http://127.0.0.1:8000/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::None,
                kind: CredentialKind::None,
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
    fixture.config.default_model = route_ref("vllm/test-model");
    assert!(
        fixture.config.default_provider_ready(),
        "explicitly credential-free providers should be considered ready"
    );
}

#[test]
fn default_provider_ready_false_when_provider_missing() {
    let mut fixture = test_app_config("openai/gpt-4o", &[]);
    fixture.config.default_model = route_ref("nonexistent/model");
    assert!(
        !fixture.config.default_provider_ready(),
        "missing provider should not be ready"
    );
}

#[test]
fn diagnostics_writer_config_resolves_runtime_values() {
    let config: HolonConfigFile = serde_json::from_value(json!({
        "runtime": {
            "diagnostics": {
                "queue_capacity": 17,
                "slow_trace_threshold_ms": 2500,
                "sample_rate_permyriad": 1250,
                "retention_age_days": 3,
                "retention_max_traces": 400,
                "retention_min_traces": 40,
                "retention_delete_batch": 25
            }
        }
    }))
    .unwrap();

    let resolved = super::resolve_diagnostics_writer_config(&config).unwrap();

    assert_eq!(resolved.queue_capacity, 17);
    assert_eq!(
        resolved.slow_trace_threshold,
        std::time::Duration::from_millis(2500)
    );
    assert_eq!(resolved.sample_rate_permyriad, 1250);
    assert_eq!(
        resolved.retention_age,
        std::time::Duration::from_secs(3 * 24 * 60 * 60)
    );
    assert_eq!(resolved.retention_max_traces, 400);
    assert_eq!(resolved.retention_min_traces, 40);
    assert_eq!(resolved.retention_delete_batch, 25);
}

#[test]
fn diagnostics_writer_config_rejects_invalid_values() {
    let config: HolonConfigFile = serde_json::from_value(json!({
        "runtime": {
            "diagnostics": {
                "sample_rate_permyriad": 10001
            }
        }
    }))
    .unwrap();

    let error = super::resolve_diagnostics_writer_config(&config).unwrap_err();

    assert!(error
        .to_string()
        .contains("diagnostics sample rate must not exceed 10000 permyriad"));
}

#[test]
fn diagnostics_writer_config_rejects_out_of_range_retention_age() {
    let config: HolonConfigFile = serde_json::from_value(json!({
        "runtime": {
            "diagnostics": {
                "retention_age_days": u64::MAX
            }
        }
    }))
    .unwrap();

    let error = super::resolve_diagnostics_writer_config(&config).unwrap_err();

    assert!(error
        .to_string()
        .contains("diagnostics retention age is out of range"));
}

#[test]
fn otlp_exporter_config_is_disabled_by_default_and_validates_bounds() {
    let mut config = HolonConfigFile::default();
    let credentials = CredentialStoreFile::default();

    assert!(super::resolve_otlp_exporter_config(&config, &credentials)
        .unwrap()
        .is_none());

    config.runtime.observability.otlp.enabled = Some(true);
    let error = super::resolve_otlp_exporter_config(&config, &credentials).unwrap_err();
    assert!(error
        .to_string()
        .contains("OTLP endpoint must not be empty"));

    config.runtime.observability.otlp.endpoint =
        Some("http://127.0.0.1:4318/v1/traces".to_string());
    config.runtime.observability.otlp.queue_capacity = Some(0);
    let error = super::resolve_otlp_exporter_config(&config, &credentials).unwrap_err();
    assert!(error
        .to_string()
        .contains("OTLP queue capacity must be greater than zero"));
}

#[test]
fn otlp_exporter_config_loads_authorization_from_credential_profile() {
    let mut config = HolonConfigFile::default();
    config.runtime.observability.otlp.enabled = Some(true);
    config.runtime.observability.otlp.endpoint =
        Some("http://127.0.0.1:4318/v1/traces".to_string());
    config.runtime.observability.otlp.credential_profile = Some("collector".to_string());
    config
        .runtime
        .observability
        .otlp
        .headers
        .insert("x-tenant".to_string(), "test".to_string());

    let mut credentials = CredentialStoreFile::default();
    credentials.profiles.insert(
        "collector".to_string(),
        crate::config::CredentialProfileFile {
            kind: CredentialKind::BearerToken,
            material: "secret-token".to_string(),
        },
    );

    let resolved = super::resolve_otlp_exporter_config(&config, &credentials)
        .unwrap()
        .unwrap();
    assert_eq!(
        resolved.headers.get("authorization").map(String::as_str),
        Some("Bearer secret-token")
    );
    assert_eq!(
        resolved.headers.get("x-tenant").map(String::as_str),
        Some("test")
    );

    config
        .runtime
        .observability
        .otlp
        .headers
        .insert("Authorization".to_string(), "inline-secret".to_string());
    let error = super::resolve_otlp_exporter_config(&config, &credentials).unwrap_err();
    assert!(error
        .to_string()
        .contains("must not configure authorization when credential_profile is set"));
}

#[test]
fn retired_scheduler_selector_accepts_only_canonical_compatibility_value() {
    let mut config = HolonConfigFile::default();
    super::validate_retired_scheduler_selector_values(None, None).unwrap();
    config.runtime.scheduler = Some("legacy".into());
    super::validate_retired_scheduler_selector_values(Some("canonical"), Some("legacy")).unwrap();
    let legacy = super::validate_retired_scheduler_selector_values(
        None,
        config.runtime.scheduler.as_deref(),
    )
    .unwrap_err();
    assert!(legacy.to_string().contains("v0.31.1"));
    let obsolete =
        super::validate_retired_scheduler_selector_values(Some("shadow"), None).unwrap_err();
    assert!(obsolete.to_string().contains("is obsolete"));
}
