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
    built_in_provider_doc_entries, built_in_provider_registry_with_settings, config_schema,
    credential_store_path, default_api_cors_allowed_headers, default_api_cors_allowed_methods,
    default_holon_home, get_config_key, get_config_value, list_credential_profiles_at,
    load_persisted_config_at, parse_anthropic_cache_strategy, parse_anthropic_cache_strategy_env,
    parse_comma_separated_values, parse_url_value, persisted_config_path,
    provider_registry_for_tests, resolve_anthropic_context_management_config,
    save_persisted_config_at, set_config_key, set_credential_profile_at, unset_config_key,
    validate_provider_config, AnthropicCacheStrategy, AnthropicContextManagementConfig, AppConfig,
    ControlAuthMode, CredentialKind, CredentialSource, CredentialStoreFile, HolonConfigFile,
    ModelConfigFile, ModelRef, ModelRouteCapability, ModelsConfigFile, ProviderAuthConfig,
    ProviderBuiltinWebSearchConfig, ProviderConfigFile, ProviderEndpointConfigFile,
    ProviderEndpointId, ProviderId, ProviderPlanConfigFile, ProviderRegistry,
    ProviderRuntimeConfig, ProviderTransportKind, RuntimeModelCatalog, DEFAULT_LOCAL_AGENT_ID,
    OPENAI_CODEX_CREDENTIAL_PROFILE,
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

fn test_app_config(default_model: &str, fallback_models: &[&str]) -> TestAppConfigFixture {
    let home_dir = tempdir().unwrap();
    let workspace_dir = tempdir().unwrap();
    let home_path = home_dir.path().to_path_buf();
    let workspace_path = workspace_dir.path().to_path_buf();
    let config = AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
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
        api_cors: Default::default(),
        config_file_path: home_path.join("config.json"),
        stored_config: Default::default(),
        default_model: ModelRef::parse(default_model).unwrap(),
        fallback_models: fallback_models
            .iter()
            .map(|value| ModelRef::parse(value).unwrap())
            .collect(),
        vision_model: None,
        image_generation_model: None,
        vision_candidate_models: Vec::new(),
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
        max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
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
fn built_in_openai_codex_defaults_to_holon_oauth_profile() {
    let settings_env = HashMap::new();
    let registry = built_in_provider_registry_with_settings(&settings_env).unwrap();
    let openai_codex = registry.get(&ProviderId::openai_codex()).unwrap();

    assert_eq!(openai_codex.auth.source, CredentialSource::AuthProfile);
    assert_eq!(openai_codex.auth.kind, CredentialKind::OAuth);
    assert_eq!(
        openai_codex.auth.profile.as_deref(),
        Some(OPENAI_CODEX_CREDENTIAL_PROFILE)
    );
    assert_eq!(openai_codex.auth.external.as_deref(), Some("codex_cli"));
    assert!(openai_codex.credential.is_none());
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
                    transport: ProviderTransportKind::OpenAiCodexResponses,
                    base_url: "https://chatgpt.com/backend-api/codex".into(),
                    auth: ProviderAuthConfig {
                        source: CredentialSource::ExternalCli,
                        kind: CredentialKind::SessionToken,
                        env: None,
                        profile: None,
                        external: Some("codex_cli".into()),
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
fn control_auth_mode_parses_known_values() {
    assert_eq!(
        ControlAuthMode::parse("auto").unwrap(),
        ControlAuthMode::Auto
    );
    assert_eq!(
        ControlAuthMode::parse("required").unwrap(),
        ControlAuthMode::Required
    );
    assert_eq!(
        ControlAuthMode::parse("disabled").unwrap(),
        ControlAuthMode::Disabled
    );
}

#[test]
fn anthropic_cache_strategy_parses_supported_values() {
    assert_eq!(
        parse_anthropic_cache_strategy("messages_native").unwrap(),
        AnthropicCacheStrategy::MessagesNative
    );
    assert_eq!(
        parse_anthropic_cache_strategy("claude-code-prompt-cache").unwrap(),
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
    assert_eq!(
        parse_anthropic_cache_strategy("current").unwrap(),
        AnthropicCacheStrategy::MessagesNative
    );
    assert_eq!(
        parse_anthropic_cache_strategy("claude-cli-like").unwrap(),
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
    let err = parse_anthropic_cache_strategy("unknown")
        .err()
        .expect("unknown strategy should fail");
    assert!(err.to_string().contains("messages_native"));
    assert!(err.to_string().contains("claude_code_prompt_cache"));
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
fn anthropic_runtime_cache_strategy_empty_env_uses_default() {
    assert_eq!(
        parse_anthropic_cache_strategy_env("").unwrap(),
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
    assert_eq!(
        parse_anthropic_cache_strategy_env("  ").unwrap(),
        AnthropicCacheStrategy::ClaudeCodePromptCache
    );
}

#[test]
fn anthropic_context_management_struct_default_stays_neutral() {
    assert_eq!(
        AnthropicContextManagementConfig::default().cache_strategy,
        AnthropicCacheStrategy::MessagesNative
    );
    assert!(AnthropicContextManagementConfig::default().betas.is_empty());
}

#[test]
fn comma_separated_values_drop_empty_items() {
    assert_eq!(
        parse_comma_separated_values(" claude-code-20250219, ,prompt-caching-scope-2026-01-05 "),
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
fn set_get_and_unset_round_trip_model_default() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "model.default", "openai-codex/gpt-5.4").unwrap();
    assert_eq!(
        get_config_key(&config, "model.default").unwrap(),
        json!("openai-codex/gpt-5.4")
    );

    unset_config_key(&mut config, "model.default").unwrap();
    assert_eq!(
        get_config_key(&config, "model.default").unwrap(),
        Value::Null
    );
}

#[test]
fn set_get_and_unset_round_trip_runtime_disable_provider_fallback() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "runtime.disable_provider_fallback", "true").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.disable_provider_fallback").unwrap(),
        Value::Bool(true)
    );

    unset_config_key(&mut config, "runtime.disable_provider_fallback").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.disable_provider_fallback").unwrap(),
        Value::Null
    );
}

#[test]
fn set_get_and_unset_round_trip_api_cors_config() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "api.cors.enabled", "true").unwrap();
    set_config_key(
        &mut config,
        "api.cors.allowed_origins",
        r#"["http://192.168.1.10:5173"]"#,
    )
    .unwrap();
    set_config_key(&mut config, "api.cors.allowed_methods", r#"["GET","POST"]"#).unwrap();
    set_config_key(
        &mut config,
        "api.cors.allowed_headers",
        r#"["content-type","authorization"]"#,
    )
    .unwrap();
    set_config_key(&mut config, "api.cors.allow_credentials", "false").unwrap();
    set_config_key(&mut config, "api.cors.max_age_seconds", "120").unwrap();

    assert_eq!(
        get_config_key(&config, "api.cors.enabled").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_origins").unwrap(),
        json!(["http://192.168.1.10:5173"])
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_methods").unwrap(),
        json!(["GET", "POST"])
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_headers").unwrap(),
        json!(["content-type", "authorization"])
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allow_credentials").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        get_config_key(&config, "api.cors.max_age_seconds").unwrap(),
        json!(120)
    );

    unset_config_key(&mut config, "api.cors.enabled").unwrap();
    unset_config_key(&mut config, "api.cors.allowed_origins").unwrap();
    unset_config_key(&mut config, "api.cors.allowed_methods").unwrap();
    unset_config_key(&mut config, "api.cors.allowed_headers").unwrap();
    unset_config_key(&mut config, "api.cors.allow_credentials").unwrap();
    unset_config_key(&mut config, "api.cors.max_age_seconds").unwrap();
    assert_eq!(
        get_config_key(&config, "api.cors.enabled").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_origins").unwrap(),
        json!([])
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_methods").unwrap(),
        json!(default_api_cors_allowed_methods())
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allowed_headers").unwrap(),
        json!(default_api_cors_allowed_headers())
    );
    assert_eq!(
        get_config_key(&config, "api.cors.allow_credentials").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "api.cors.max_age_seconds").unwrap(),
        json!(600)
    );
}

#[test]
fn api_cors_rejects_credentials_with_wildcard_origin() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "api.cors.allowed_origins", r#"["*"]"#).unwrap();

    let error = set_config_key(&mut config, "api.cors.allow_credentials", "true")
        .expect_err("credentials plus wildcard origin should be rejected");

    assert!(
        error.to_string().contains("allow_credentials"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn set_get_and_unset_round_trip_tool_output_budgets() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "runtime.default_tool_output_tokens", "1500").unwrap();
    set_config_key(&mut config, "runtime.max_tool_output_tokens", "6000").unwrap();

    assert_eq!(
        get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
        json!(1_500)
    );
    assert_eq!(
        get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
        json!(6_000)
    );

    set_config_key(&mut config, "runtime.default_tool_output_tokens", "100000").unwrap();
    set_config_key(&mut config, "runtime.max_tool_output_tokens", "100000").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
        json!(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS)
    );
    assert_eq!(
        get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
        json!(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS)
    );

    unset_config_key(&mut config, "runtime.default_tool_output_tokens").unwrap();
    unset_config_key(&mut config, "runtime.max_tool_output_tokens").unwrap();
    assert_eq!(
        get_config_key(&config, "runtime.default_tool_output_tokens").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "runtime.max_tool_output_tokens").unwrap(),
        Value::Null
    );
}

#[test]
fn set_get_and_unset_round_trip_web_config() {
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "web.fetch.enabled", "true").unwrap();
    set_config_key(
        &mut config,
        "web.fetch.allowed_hosts",
        "localhost:3000,127.0.0.1:5173",
    )
    .unwrap();
    set_config_key(&mut config, "web.search.provider", "duckduckgo").unwrap();
    set_config_key(&mut config, "web.search.builtin_provider.enabled", "false").unwrap();
    set_config_key(&mut config, "web.search.max_results", "3").unwrap();
    set_config_key(&mut config, "web.search.mode", "aggregate").unwrap();
    set_config_key(&mut config, "web.search.providers", "searx,brave").unwrap();
    set_config_key(&mut config, "web.search.max_provider_attempts", "2").unwrap();
    set_config_key(&mut config, "web.providers.brave.kind", "brave").unwrap();
    set_config_key(&mut config, "web.fetch.max_response_bytes", "12345").unwrap();
    set_config_key(&mut config, "web.fetch.timeout_seconds", "7").unwrap();
    set_config_key(&mut config, "web.fetch.max_redirects", "2").unwrap();

    assert_eq!(
        get_config_key(&config, "web.fetch.enabled").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.allowed_hosts").unwrap(),
        json!(["localhost:3000", "127.0.0.1:5173"])
    );
    assert_eq!(
        get_config_key(&config, "web.search.provider").unwrap(),
        json!("duckduckgo")
    );
    assert_eq!(
        get_config_key(&config, "web.search.builtin_provider.enabled").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        get_config_key(&config, "web.search.max_results").unwrap(),
        json!(3)
    );
    assert_eq!(
        get_config_key(&config, "web.search.mode").unwrap(),
        json!("aggregate")
    );
    assert_eq!(
        get_config_key(&config, "web.search.providers").unwrap(),
        json!(["searx", "brave"])
    );
    assert_eq!(
        get_config_key(&config, "web.search.max_provider_attempts").unwrap(),
        json!(2)
    );
    assert_eq!(
        get_config_key(&config, "web.providers.searx.capabilities").unwrap(),
        Value::Null
    );
    let capabilities = get_config_key(&config, "web.providers.brave.capabilities").unwrap();
    assert_eq!(capabilities["auth"], json!("api_key"));
    assert_eq!(capabilities["status"], json!("supported"));
    assert_eq!(capabilities["default_priority"], json!(80));
    let read_only_capabilities_error =
            "web.providers.brave.capabilities is derived read-only capability metadata; configure web.providers.<name>.kind instead";
    assert_eq!(
        set_config_key(
            &mut config,
            "web.providers.brave.capabilities",
            r#"{"status":"supported"}"#
        )
        .unwrap_err()
        .to_string(),
        read_only_capabilities_error
    );
    assert_eq!(
        unset_config_key(&mut config, "web.providers.brave.capabilities")
            .unwrap_err()
            .to_string(),
        read_only_capabilities_error
    );
    assert_eq!(
        get_config_key(&config, "web.providers.brave.kind").unwrap(),
        json!("brave")
    );
    assert_eq!(
        get_config_key(&config, "web.providers.brave.base_url").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.max_response_bytes").unwrap(),
        json!(12_345)
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.timeout_seconds").unwrap(),
        json!(7)
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.max_redirects").unwrap(),
        json!(2)
    );

    unset_config_key(&mut config, "web.fetch.allowed_hosts").unwrap();
    unset_config_key(&mut config, "web.search.provider").unwrap();
    unset_config_key(&mut config, "web.search.builtin_provider.enabled").unwrap();
    unset_config_key(&mut config, "web.search.mode").unwrap();
    unset_config_key(&mut config, "web.search.providers").unwrap();
    unset_config_key(&mut config, "web.search.max_provider_attempts").unwrap();
    unset_config_key(&mut config, "web.providers.brave").unwrap();
    unset_config_key(&mut config, "web.fetch.max_response_bytes").unwrap();
    unset_config_key(&mut config, "web.fetch.timeout_seconds").unwrap();
    unset_config_key(&mut config, "web.fetch.max_redirects").unwrap();
    assert_eq!(
        get_config_key(&config, "web.fetch.allowed_hosts").unwrap(),
        json!([])
    );
    assert_eq!(
        get_config_key(&config, "web.search.provider").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.search.builtin_provider.enabled").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.search.mode").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.search.providers").unwrap(),
        json!([])
    );
    assert_eq!(
        get_config_key(&config, "web.search.max_provider_attempts").unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "web.fetch.max_response_bytes").unwrap(),
        Value::Null
    );
}

#[test]
fn set_get_and_unset_round_trip_unknown_model_fallback_field() {
    let mut config = HolonConfigFile::default();
    set_config_key(
        &mut config,
        "model.unknown_fallback.prompt_budget_estimated_tokens",
        "64000",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "model.unknown_fallback.runtime_max_output_tokens",
        "4096",
    )
    .unwrap();
    assert_eq!(
        get_config_key(
            &config,
            "model.unknown_fallback.prompt_budget_estimated_tokens"
        )
        .unwrap(),
        json!(64_000)
    );
    assert_eq!(
        get_config_key(&config, "model.unknown_fallback.runtime_max_output_tokens").unwrap(),
        json!(4_096)
    );

    unset_config_key(
        &mut config,
        "model.unknown_fallback.prompt_budget_estimated_tokens",
    )
    .unwrap();
    assert_eq!(
        get_config_key(
            &config,
            "model.unknown_fallback.prompt_budget_estimated_tokens"
        )
        .unwrap(),
        Value::Null
    );
    assert_eq!(
        get_config_key(&config, "model.unknown_fallback.runtime_max_output_tokens").unwrap(),
        json!(4_096)
    );

    unset_config_key(
        &mut config,
        "model.unknown_fallback.runtime_max_output_tokens",
    )
    .unwrap();
    assert_eq!(
        get_config_key(&config, "model.unknown_fallback").unwrap(),
        Value::Null
    );
}

#[test]
fn built_in_provider_registry_declares_provider_specific_builtin_search() {
    let registry = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();

    let openai_codex = registry.get(&ProviderId::openai_codex()).unwrap();
    let openai_codex_search = openai_codex.builtin_web_search.as_ref().unwrap();
    assert_eq!(
        openai_codex_search.kind,
        ProviderNativeWebSearchKind::OpenAi
    );
    assert_eq!(openai_codex_search.advertised_tool_type, "web_search");
    assert_eq!(openai_codex_search.backend_kind, "openai_codex_web_search");

    let anthropic = registry.get(&ProviderId::anthropic()).unwrap();
    let anthropic_search = anthropic.builtin_web_search.as_ref().unwrap();
    assert_eq!(
        anthropic_search.kind,
        ProviderNativeWebSearchKind::Anthropic
    );
    assert_eq!(anthropic_search.advertised_tool_type, "web_search_20250305");
    assert_eq!(anthropic_search.backend_kind, "anthropic_web_search");

    let zai = registry.get(&ProviderId::parse("zai").unwrap()).unwrap();
    let zai_search = zai.builtin_web_search.as_ref().unwrap();
    assert_eq!(zai_search.kind, ProviderNativeWebSearchKind::Anthropic);
    assert_eq!(zai_search.advertised_tool_type, "web_search_20250305");
    assert_eq!(zai_search.backend_kind, "zai_web_search_prime");

    let deepseek = registry
        .get(&ProviderId::parse("deepseek").unwrap())
        .unwrap();
    let deepseek_search = deepseek.builtin_web_search.as_ref().unwrap();
    assert_eq!(deepseek_search.kind, ProviderNativeWebSearchKind::Anthropic);
    assert_eq!(deepseek_search.advertised_tool_type, "web_search_20250305");
    assert_eq!(deepseek_search.backend_kind, "deepseek_web_search");

    let xai = registry.get(&ProviderId::parse("xai").unwrap()).unwrap();
    assert_eq!(xai.transport, ProviderTransportKind::OpenAiResponses);
    assert_eq!(xai.reasoning_effort.as_deref(), Some("medium"));
    let xai_search = xai.builtin_web_search.as_ref().unwrap();
    assert_eq!(xai_search.kind, ProviderNativeWebSearchKind::Xai);
    assert_eq!(xai_search.advertised_tool_type, "web_search");
    assert_eq!(xai_search.backend_kind, "xai_web_search_x_search");
}

#[test]
fn set_get_and_unset_round_trip_models_catalog_object() {
    let mut config = HolonConfigFile::default();
    set_config_key(
            &mut config,
            "models.catalog",
            r#"{"anthropic/claude-sonnet-4-6":{"prompt_budget_estimated_tokens":32000,"capabilities":{"image_input":true}}}"#,
        )
        .unwrap();
    assert_eq!(
        get_config_key(&config, "models.catalog").unwrap(),
        json!({
            "anthropic/claude-sonnet-4-6": {
                "capabilities": {
                    "image_input": true
                },
                "prompt_budget_estimated_tokens": 32_000
            }
        })
    );

    unset_config_key(&mut config, "models.catalog").unwrap();
    assert_eq!(
        get_config_key(&config, "models.catalog").unwrap(),
        json!({})
    );
}

#[test]
fn models_catalog_rejects_invalid_model_refs() {
    let mut config = HolonConfigFile::default();
    let err = set_config_key(
        &mut config,
        "models.catalog",
        r#"{"gpt-5.4":{"prompt_budget_estimated_tokens":32000}}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("expected provider/model"));
}

#[test]
fn custom_provider_auth_requires_explicit_contract() {
    let id = ProviderId::parse("openrouter").unwrap();
    let auth = ProviderAuthConfig {
        source: CredentialSource::Env,
        kind: CredentialKind::ApiKey,
        env: None,
        profile: None,
        external: None,
    };
    let err = super::validate_provider_auth(&id, &auth).unwrap_err();
    assert!(err.to_string().contains("requires auth.env"));
}

#[test]
fn provider_builtin_web_search_rejects_empty_tool_metadata() {
    let id = ProviderId::parse("custom-anthropic").unwrap();
    let config = ProviderConfigFile {
        transport: ProviderTransportKind::AnthropicMessages,
        base_url: "https://api.example.com".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::Env,
            kind: CredentialKind::ApiKey,
            env: Some("CUSTOM_API_KEY".into()),
            profile: None,
            external: None,
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::Anthropic,
            advertised_tool_type: String::new(),
            backend_kind: "custom_backend".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    let err = validate_provider_config(&id, &config).unwrap_err();
    assert!(err.to_string().contains("advertised_tool_type"));
    assert!(err.to_string().contains("must not be empty"));
}

#[test]
fn provider_builtin_web_search_rejects_transport_kind_mismatch() {
    let id = ProviderId::parse("custom-openai").unwrap();
    let config = ProviderConfigFile {
        transport: ProviderTransportKind::OpenAiResponses,
        base_url: "https://api.example.com".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::Env,
            kind: CredentialKind::ApiKey,
            env: Some("CUSTOM_API_KEY".into()),
            profile: None,
            external: None,
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::Anthropic,
            advertised_tool_type: "web_search_20250305".into(),
            backend_kind: "custom_backend".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    let err = validate_provider_config(&id, &config).unwrap_err();
    assert!(err.to_string().contains("incompatible with transport"));
}

#[test]
fn provider_builtin_web_search_rejects_wrong_tool_type() {
    let id = ProviderId::parse("custom-openai").unwrap();
    let config = ProviderConfigFile {
        transport: ProviderTransportKind::OpenAiResponses,
        base_url: "https://api.example.com".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::Env,
            kind: CredentialKind::ApiKey,
            env: Some("CUSTOM_API_KEY".into()),
            profile: None,
            external: None,
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::OpenAi,
            advertised_tool_type: "web_search_20250305".into(),
            backend_kind: "custom_backend".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    let err = validate_provider_config(&id, &config).unwrap_err();
    assert!(err.to_string().contains("web_search_preview"));
}

#[test]
fn provider_builtin_web_search_accepts_codex_tool_type() {
    let id = ProviderId::openai_codex();
    let config = ProviderConfigFile {
        transport: ProviderTransportKind::OpenAiCodexResponses,
        base_url: "https://chatgpt.com/backend-api/codex".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::ExternalCli,
            kind: CredentialKind::SessionToken,
            env: None,
            profile: None,
            external: Some("codex_cli".into()),
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::OpenAi,
            advertised_tool_type: "web_search".into(),
            backend_kind: "openai_codex_web_search".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    validate_provider_config(&id, &config).unwrap();
}

#[test]
fn provider_builtin_web_search_rejects_wrong_codex_tool_type() {
    let id = ProviderId::openai_codex();
    let config = ProviderConfigFile {
        transport: ProviderTransportKind::OpenAiCodexResponses,
        base_url: "https://chatgpt.com/backend-api/codex".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::ExternalCli,
            kind: CredentialKind::SessionToken,
            env: None,
            profile: None,
            external: Some("codex_cli".into()),
        },
        reasoning_effort: None,
        builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
            enabled: true,
            kind: ProviderNativeWebSearchKind::OpenAi,
            advertised_tool_type: "web_search_preview".into(),
            backend_kind: "openai_codex_web_search".into(),
        }),
        endpoints: BTreeMap::new(),
        plans: BTreeMap::new(),
    };

    let err = validate_provider_config(&id, &config).unwrap_err();
    assert!(err.to_string().contains("web_search for OpenAI Codex"));
}

#[test]
fn materialize_provider_config_can_disable_builtin_web_search_for_builtin_provider() {
    let id = ProviderId::openai_codex();
    let built_in = built_in_provider_registry_with_settings(&HashMap::new())
        .unwrap()
        .remove(&id)
        .unwrap();
    assert!(built_in.builtin_web_search.is_some());

    let runtime = super::materialize_provider_config(
        id.clone(),
        ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::ExternalCli,
                kind: CredentialKind::SessionToken,
                env: None,
                profile: None,
                external: Some("codex_cli".into()),
            },
            reasoning_effort: None,
            builtin_web_search: Some(ProviderBuiltinWebSearchConfig {
                enabled: false,
                kind: ProviderNativeWebSearchKind::OpenAi,
                advertised_tool_type: "web_search".into(),
                backend_kind: "openai_codex_web_search".into(),
            }),
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &HashMap::new(),
        &CredentialStoreFile::default(),
        Some(built_in),
    )
    .unwrap();

    assert_eq!(runtime.id, id);
    assert!(runtime.builtin_web_search.is_none());
}

#[test]
fn materialize_provider_config_resolves_env_credentials_from_settings() {
    let mut settings_env = HashMap::new();
    settings_env.insert("OPENROUTER_API_KEY".to_string(), "settings-key".to_string());
    let id = ProviderId::parse("openrouter").unwrap();
    let runtime = super::materialize_provider_config(
        id.clone(),
        ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://openrouter.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENROUTER_API_KEY".into()),
                profile: None,
                external: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &settings_env,
        &CredentialStoreFile::default(),
        None,
    )
    .unwrap();

    assert_eq!(runtime.id, id);
    assert_eq!(runtime.credential.as_deref(), Some("settings-key"));
}

#[test]
fn credential_source_accepts_credential_profile_alias() {
    assert_eq!(
        CredentialSource::parse("credential_profile").unwrap(),
        CredentialSource::AuthProfile
    );
    assert_eq!(
        CredentialSource::parse("auth_profile").unwrap(),
        CredentialSource::AuthProfile
    );
    assert_eq!(CredentialSource::AuthProfile.as_str(), "credential_profile");
}

#[test]
fn credential_store_lists_profiles_without_raw_material() {
    let dir = tempdir().unwrap();
    let path = credential_store_path(dir.path());

    let status = set_credential_profile_at(
        &path,
        "openai:default",
        CredentialKind::ApiKey,
        "sk-test-value".into(),
    )
    .unwrap();

    assert_eq!(status.profile, "openai:default");
    let profiles = list_credential_profiles_at(&path).unwrap();
    assert_eq!(
        serde_json::to_value(&profiles).unwrap(),
        json!([{
            "profile": "openai:default",
            "kind": "api_key",
            "configured": true
        }])
    );
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.contains("sk-test-value"));
    assert!(!serde_json::to_string(&profiles)
        .unwrap()
        .contains("sk-test-value"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn materialize_provider_config_resolves_credential_profile() {
    let settings_env = HashMap::new();
    let id = ProviderId::parse("openrouter").unwrap();
    let mut credential_store = CredentialStoreFile::default();
    credential_store.profiles.insert(
        "openrouter:default".into(),
        super::CredentialProfileFile {
            kind: CredentialKind::ApiKey,
            material: "profile-value".into(),
        },
    );

    let runtime = super::materialize_provider_config(
        id.clone(),
        ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://openrouter.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::ApiKey,
                env: None,
                profile: Some(" openrouter:default ".into()),
                external: None,
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &settings_env,
        &credential_store,
        None,
    )
    .unwrap();

    assert_eq!(runtime.id, id);
    assert_eq!(runtime.credential.as_deref(), Some("profile-value"));
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
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://openrouter.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::ApiKey,
                env: None,
                profile: Some("openrouter:default".into()),
                external: None,
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
        "VOLCENGINE_AGENT_API_KEY".to_string(),
        "volcengine-agent-key".to_string(),
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

    let volcengine_agent = providers
        .get(&ProviderId::parse("volcengine-agent").unwrap())
        .unwrap();
    assert_eq!(
        volcengine_agent.transport,
        ProviderTransportKind::OpenAiResponses
    );
    assert_eq!(volcengine_agent.base_url, "https://ark.example/api/v3");
    assert_eq!(
        volcengine_agent.auth.env.as_deref(),
        Some("VOLCENGINE_AGENT_API_KEY")
    );
    assert_eq!(
        volcengine_agent.credential.as_deref(),
        Some("volcengine-agent-key")
    );
    // Backward compat: VOLCENGINE_IMAGE_OPENAI_API_KEY still works as fallback.
    let mut volcengine_image_openai_only_env = HashMap::new();
    volcengine_image_openai_only_env.insert(
        "VOLCENGINE_IMAGE_OPENAI_API_KEY".to_string(),
        "volcengine-image-key".to_string(),
    );
    let volcengine_image_openai_only_providers =
        super::built_in_provider_registry_with_settings(&volcengine_image_openai_only_env).unwrap();
    let volcengine_agent = volcengine_image_openai_only_providers
        .get(&ProviderId::parse("volcengine-agent").unwrap())
        .unwrap();
    assert_eq!(
        volcengine_agent.auth.env.as_deref(),
        Some("VOLCENGINE_IMAGE_OPENAI_API_KEY")
    );
    assert_eq!(
        volcengine_agent.credential.as_deref(),
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
        stepfun_plan.auth.env.as_deref(),
        Some("STEPFUN_PLAN_API_KEY or STEPFUN_API_KEY")
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
    assert_eq!(
        xiaomi.transport,
        ProviderTransportKind::OpenAiChatCompletions
    );
    assert_eq!(xiaomi.base_url, "https://api.xiaomimimo.com/v1");
    assert_eq!(xiaomi.credential.as_deref(), Some("xiaomi-key"));

    let xiaomi_token_plan = providers
        .get(&ProviderId::parse("xiaomi-token-plan").unwrap())
        .unwrap();
    assert_eq!(
        xiaomi_token_plan.transport,
        ProviderTransportKind::OpenAiChatCompletions
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
fn vercel_ai_gateway_prefers_oidc_bearer_token_over_api_key() {
    let mut settings_env = HashMap::new();
    settings_env.insert("VERCEL_OIDC_TOKEN".to_string(), "oidc-token".to_string());
    settings_env.insert("AI_GATEWAY_API_KEY".to_string(), "api-key".to_string());

    let providers = super::built_in_provider_registry_with_settings(&settings_env).unwrap();
    let vercel_ai_gateway = providers
        .get(&ProviderId::parse("vercel-ai-gateway").unwrap())
        .unwrap();

    assert_eq!(
        vercel_ai_gateway.transport,
        ProviderTransportKind::AnthropicMessages
    );
    assert_eq!(vercel_ai_gateway.auth.source, CredentialSource::Env);
    assert_eq!(vercel_ai_gateway.auth.kind, CredentialKind::BearerToken);
    assert_eq!(
        vercel_ai_gateway.auth.env.as_deref(),
        Some("VERCEL_OIDC_TOKEN")
    );
    assert_eq!(vercel_ai_gateway.credential.as_deref(), Some("oidc-token"));
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
fn built_in_provider_default_config_resolves_known_and_unknown_provider() {
    let settings_env = HashMap::new();

    let known = super::built_in_provider_default_config_with_settings(
        &ProviderId::parse("zai").unwrap(),
        &settings_env,
    )
    .unwrap()
    .expect("expected built-in default");
    assert_eq!(known.transport, ProviderTransportKind::AnthropicMessages);
    assert_eq!(known.base_url, "https://api.z.ai/api/anthropic");
    assert_eq!(known.auth.source, CredentialSource::None);
    assert_eq!(known.auth.kind, CredentialKind::None);
    assert!(known.auth.env.is_none());

    let unknown = super::built_in_provider_default_config_with_settings(
        &ProviderId::parse("unknown-provider").unwrap(),
        &settings_env,
    )
    .unwrap();
    assert!(unknown.is_none());
}

#[test]
fn materialize_provider_config_preserves_builtin_runtime_fields() {
    let settings_env = HashMap::new();
    let mut built_ins = super::built_in_provider_registry_with_settings(&settings_env).unwrap();
    let id = ProviderId::openai_codex();
    let built_in = built_ins.remove(&id).unwrap();

    let runtime = super::materialize_provider_config(
        id,
        ProviderConfigFile {
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://codex.example/backend-api".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::ExternalCli,
                kind: CredentialKind::SessionToken,
                env: None,
                profile: None,
                external: Some("codex_cli".into()),
            },
            reasoning_effort: None,
            builtin_web_search: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        },
        &settings_env,
        &CredentialStoreFile::default(),
        Some(built_in),
    )
    .unwrap();

    assert_eq!(runtime.base_url, "https://codex.example/backend-api");
    assert!(runtime.codex_home.is_some());
    assert_eq!(runtime.originator.as_deref(), Some("codex_cli_rs"));
    assert_eq!(runtime.reasoning_effort.as_deref(), Some("low"));
}

#[test]
fn model_selection_explicit_config_wins() {
    let providers = provider_registry_for_tests(
        Some("openai-key"),
        Some("anthropic-token"),
        tempdir().unwrap().path().join("codex-home"),
    );

    let (default_model, fallback_models) = super::resolve_model_selection_from_explicit(
        Some(ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap()),
        Some(vec![
            ModelRef::parse("openai/gpt-5.4").unwrap(),
            ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
        ]),
        &providers,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(default_model.as_string(), "anthropic/claude-sonnet-4-6");
    assert_eq!(
        fallback_models
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>(),
        vec!["openai/gpt-5.4"]
    );
}

#[test]
fn model_selection_derives_from_authenticated_providers() {
    let providers = provider_registry_for_tests(
        Some("openai-key"),
        Some("anthropic-token"),
        tempdir().unwrap().path().join("codex-home"),
    );

    let (default_model, fallback_models) =
        super::resolve_model_selection_from_explicit(None, None, &providers, &HashMap::new())
            .unwrap();

    assert_eq!(default_model.as_string(), "openai/gpt-5.4");
    assert_eq!(
        fallback_models
            .into_iter()
            .map(|model| model.as_string())
            .collect::<Vec<_>>(),
        vec!["anthropic/claude-sonnet-4-6"]
    );
}

#[test]
fn model_selection_fails_without_explicit_config_or_auth() {
    let providers =
        provider_registry_for_tests(None, None, tempdir().unwrap().path().join("codex-home"));

    let err = super::resolve_model_selection_from_explicit(None, None, &providers, &HashMap::new())
        .unwrap_err();

    assert!(err.to_string().contains("no default model configured"));
    assert!(err.to_string().contains("configure provider credentials"));
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
                    transport: ProviderTransportKind::OpenAiChatCompletions,
                    base_url: "https://custom.example/v1".into(),
                    auth: ProviderAuthConfig {
                        source: CredentialSource::Env,
                        kind: CredentialKind::ApiKey,
                        env: Some("HOLON_TEST_MISSING_CUSTOM_OPENAI_API_KEY".into()),
                        profile: None,
                        external: None,
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
    assert_eq!(config.default_model.as_string(), "openai/unknown");
}

#[test]
fn model_selection_derives_custom_provider_from_catalog_override() {
    let mut providers = ProviderRegistry::new();
    let id = ProviderId::parse("custom-openai").unwrap();
    providers.insert(
        id.clone(),
        ProviderRuntimeConfig {
            id: id.clone(),
            route_provider: id.clone(),
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiChatCompletions,
            base_url: "https://custom.example/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("CUSTOM_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: Some("custom-key".into()),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );
    let mut overrides = HashMap::new();
    overrides.insert(
        ModelRef::parse("custom-openai/model-b").unwrap(),
        ModelRuntimeOverride::default(),
    );
    overrides.insert(
        ModelRef::parse("custom-openai/model-a").unwrap(),
        ModelRuntimeOverride::default(),
    );

    let (default_model, fallback_models) =
        super::resolve_model_selection_from_explicit(None, None, &providers, &overrides).unwrap();

    assert_eq!(default_model.as_string(), "custom-openai/model-a");
    assert!(fallback_models.is_empty());
}

#[test]
fn url_parser_rejects_invalid_or_non_http_urls() {
    let err = parse_url_value("providers.openai.base_url", "").unwrap_err();
    assert!(err.to_string().contains("non-empty URL"));

    let err = parse_url_value("providers.openai.base_url", "notaurl").unwrap_err();
    assert!(err.to_string().contains("valid absolute URL"));

    let err = parse_url_value("providers.openai.base_url", "ftp://example.com").unwrap_err();
    assert!(err.to_string().contains("http or https URL"));

    parse_url_value("providers.openai.base_url", "https://api.openai.com/v1").unwrap();
}

#[test]
fn model_ref_requires_provider_prefix() {
    let err = ModelRef::parse("gpt-5.4").unwrap_err();
    assert!(err.to_string().contains("expected provider/model"));
}

#[test]
fn model_ref_parses_provider_and_model() {
    let parsed = ModelRef::parse("openai-codex/gpt-5.4").unwrap();
    assert_eq!(parsed.provider, ProviderId::openai_codex());
    assert_eq!(parsed.model, "gpt-5.4");
    assert_eq!(parsed.as_string(), "openai-codex/gpt-5.4");
}

#[test]
fn provider_id_accepts_custom_values() {
    let parsed = ProviderId::parse("vertex").unwrap();
    assert_eq!(parsed.as_str(), "vertex");
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
            "openai/gpt-5.4",
            "anthropic/claude-sonnet-4-6",
            "openai-codex/gpt-5.4",
        ]
    );
}

#[test]
fn provider_chain_returns_only_effective_model_when_fallback_disabled() {
    let mut fixture = test_app_config(
        "openai/gpt-5.4",
        &["anthropic/claude-sonnet-4-6", "openai-codex/gpt-5.4"],
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

    assert_eq!(chain, vec!["openai/gpt-5.4"]);
}

#[test]
fn provider_chain_with_override_inserts_override_before_runtime_default() {
    let fixture = test_app_config(
        "anthropic/claude-sonnet-4-6",
        &["openai/gpt-5.4", "anthropic/claude-sonnet-4-6"],
    );

    let override_model = ModelRef::parse("openai/gpt-5.4-mini").unwrap();
    let chain = fixture
        .config
        .provider_chain_with_override(Some(&override_model))
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();

    assert_eq!(
        chain,
        vec![
            "openai/gpt-5.4-mini",
            "anthropic/claude-sonnet-4-6",
            "openai/gpt-5.4",
        ]
    );
}

#[test]
fn provider_chain_for_turn_starts_at_pending_fallback_model() {
    let fixture = test_app_config(
        "openai/gpt-5.4",
        &[
            "anthropic/claude-sonnet-4-6",
            "openai-codex/gpt-5.3-codex-spark",
        ],
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);
    let pending = ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap();

    let chain = catalog
        .provider_chain_for_turn(None, Some(&pending))
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();

    assert_eq!(
        chain,
        vec![
            "anthropic/claude-sonnet-4-6",
            "openai-codex/gpt-5.3-codex-spark",
        ]
    );
}

#[test]
fn provider_chain_with_override_returns_only_override_when_fallback_disabled() {
    let mut fixture = test_app_config(
        "anthropic/claude-sonnet-4-6",
        &["openai/gpt-5.4", "openai-codex/gpt-5.4"],
    );
    fixture
        .config
        .stored_config
        .runtime
        .disable_provider_fallback = Some(true);
    fixture.config.disable_provider_fallback = true;

    let override_model = ModelRef::parse("openai/gpt-5.4-mini").unwrap();
    let chain = fixture
        .config
        .provider_chain_with_override(Some(&override_model))
        .into_iter()
        .map(|model| model.as_string())
        .collect::<Vec<_>>();

    assert_eq!(chain, vec!["openai/gpt-5.4-mini"]);
}

#[test]
fn model_ref_serializes_as_string() {
    let model_ref = ModelRef::parse("openai/gpt-5.4").unwrap();
    let encoded = serde_json::to_value(&model_ref).unwrap();
    assert_eq!(encoded, json!("openai/gpt-5.4"));

    let decoded: ModelRef = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, model_ref);
}

#[test]
fn persisted_config_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = HolonConfigFile::default();
    set_config_key(&mut config, "model.default", "openai/gpt-5.4").unwrap();

    save_persisted_config_at(&path, &config).unwrap();
    let loaded = load_persisted_config_at(&path).unwrap();
    assert_eq!(
        get_config_key(&loaded, "model.default").unwrap(),
        json!("openai/gpt-5.4")
    );
}

#[test]
fn schema_contains_expected_keys() {
    let keys = config_schema()
        .into_iter()
        .map(|entry| entry.key)
        .collect::<Vec<_>>();
    assert!(keys.contains(&"model.default"));
    assert!(keys.contains(&"models.catalog"));
    assert!(keys.contains(&"model.unknown_fallback"));
    assert!(keys.contains(&"providers.<id>.endpoints.<endpoint_id>.transport"));
    assert!(keys.contains(&"providers.<id>.endpoints.<endpoint_id>.base_url"));
    assert!(keys.contains(&"providers.<id>.plans.<plan_id>.endpoint"));
    assert!(!keys.contains(&"providers.openai-codex.auth_source"));
    assert!(keys.contains(&"runtime.max_output_tokens"));
    assert!(keys.contains(&"runtime.default_tool_output_tokens"));
    assert!(keys.contains(&"runtime.max_tool_output_tokens"));
    assert!(keys.contains(&"runtime.disable_provider_fallback"));
    assert!(keys.contains(&"tui.alternate_screen"));
    assert!(keys.contains(&"web.fetch.enabled"));
    assert!(keys.contains(&"web.fetch.allowed_hosts"));
    assert!(keys.contains(&"web.fetch.max_response_bytes"));
    assert!(keys.contains(&"web.fetch.timeout_seconds"));
    assert!(keys.contains(&"web.fetch.max_redirects"));
    assert!(keys.contains(&"web.search.provider"));
    assert!(keys.contains(&"web.providers.<name>.capabilities"));
}

#[test]
fn load_persisted_config_reads_provider_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let legacy_payload = serde_json::json!({
        "model": {
            "default": "anthropic/claude-sonnet-4-6"
        },
        "runtime": {
            "max_output_tokens": 4096
        },
        "providers": {
            "openai": {
                "transport": "openai_responses",
                "base_url": "https://example.openai.com/v1",
                "auth": {
                    "source": "env",
                    "kind": "api_key",
                    "env": "OPENAI_API_KEY"
                }
            }
        },
        "tui": {
            "alternate_screen": "never"
        }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&legacy_payload).unwrap()).unwrap();

    let config = load_persisted_config_at(&path).unwrap();
    assert_eq!(
        config.model.default,
        Some("anthropic/claude-sonnet-4-6".to_string())
    );
    assert_eq!(config.runtime.max_output_tokens, Some(4_096));
    assert_eq!(
        config.tui.alternate_screen,
        Some(crate::config::AltScreenMode::Never)
    );
    assert_eq!(
        config
            .providers
            .get(&ProviderId::openai())
            .unwrap()
            .base_url,
        "https://example.openai.com/v1"
    );
}

#[test]
fn provider_endpoint_config_keys_round_trip_default_endpoint() {
    let mut config = HolonConfigFile::default();

    set_config_key(
        &mut config,
        "providers.openai.endpoints.default.transport",
        "openai_chat_completions",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "providers.openai.endpoints.default.base_url",
        "https://example.openai.test/v1",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "providers.openai.endpoints.default.auth.env",
        "CUSTOM_OPENAI_API_KEY",
    )
    .unwrap();
    set_config_key(
        &mut config,
        "providers.openai.plans.coding.endpoint",
        "default",
    )
    .unwrap();

    assert_eq!(
        get_config_key(&config, "providers.openai.endpoints.default.transport").unwrap(),
        json!("openai_chat_completions")
    );
    assert_eq!(
        get_config_key(&config, "providers.openai.transport").unwrap(),
        json!("openai_responses")
    );
    assert_eq!(
        get_config_key(&config, "providers.openai.endpoints.default.base_url").unwrap(),
        json!("https://example.openai.test/v1")
    );
    assert_eq!(
        get_config_key(&config, "providers.openai.endpoints.default.auth.env").unwrap(),
        json!("CUSTOM_OPENAI_API_KEY")
    );
    assert_eq!(
        get_config_key(&config, "providers.openai.plans.coding.endpoint").unwrap(),
        json!("default")
    );

    unset_config_key(&mut config, "providers.openai.endpoints.default.auth.env").unwrap();
    assert_eq!(
        get_config_key(&config, "providers.openai.endpoints.default.auth.env").unwrap(),
        json!(null)
    );
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
            },
            providers: BTreeMap::from([(
                provider_id.clone(),
                ProviderConfigFile {
                    transport: ProviderTransportKind::OpenAiResponses,
                    base_url: "https://legacy.example/v1".into(),
                    auth: ProviderAuthConfig::default(),
                    reasoning_effort: None,
                    builtin_web_search: None,
                    endpoints: BTreeMap::from([(
                        ProviderEndpointId::default_endpoint(),
                        ProviderEndpointConfigFile {
                            transport: Some(ProviderTransportKind::OpenAiChatCompletions),
                            base_url: Some("https://endpoint.example/v1".into()),
                            auth: Some(ProviderAuthConfig {
                                source: CredentialSource::None,
                                kind: CredentialKind::None,
                                env: None,
                                profile: None,
                                external: None,
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
                    transport: ProviderTransportKind::OpenAiResponses,
                    base_url: "https://default.example/v1".into(),
                    auth: ProviderAuthConfig::default(),
                    reasoning_effort: None,
                    builtin_web_search: None,
                    endpoints: BTreeMap::from([(
                        endpoint_id.clone(),
                        ProviderEndpointConfigFile {
                            transport: Some(ProviderTransportKind::OpenAiChatCompletions),
                            base_url: Some("https://coding.example/v1".into()),
                            auth: Some(ProviderAuthConfig {
                                source: CredentialSource::None,
                                kind: CredentialKind::None,
                                env: None,
                                profile: None,
                                external: None,
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
fn unknown_provider_subkeys_report_unknown_key() {
    let config = HolonConfigFile::default();
    let err = get_config_key(&config, "providers.openai.auth_source").unwrap_err();
    assert!(err.to_string().contains("unknown config key"));
}

#[test]
fn resolve_disable_provider_fallback_rejects_invalid_env_override() {
    let err = super::resolve_disable_provider_fallback_override(
        Some("maybe"),
        &HolonConfigFile::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("HOLON_DISABLE_PROVIDER_FALLBACK expects a boolean"));
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
        ..ContextConfig::default()
    };

    let known = catalog.resolved_model_policy(&base_context, None);
    assert_eq!(known.prompt_budget_estimated_tokens, 24_000);
    assert_eq!(known.runtime_max_output_tokens, 4_096);

    let unknown = catalog.resolved_model_policy(
        &base_context,
        Some(&ModelRef::parse("openai/custom-model").unwrap()),
    );
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
    fixture.config.vision_candidate_models = vec![ModelRef::parse("openai/gpt-5.4").unwrap()];
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
    fixture.config.vision_model = Some(ModelRef::parse("openai/gpt-5.4").unwrap());
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
    fixture.config.vision_candidate_models = vec![ModelRef::parse("openai/gpt-5.4-mini").unwrap()];
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
    fixture.config.vision_candidate_models = vec![custom_model.clone()];
    fixture.config.providers.insert(
        custom_model.provider.clone(),
        ProviderRuntimeConfig {
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
    let mut fixture = test_app_config("anthropic/claude-sonnet-4-6", &[]);
    fixture.config.vision_candidate_models =
        vec![ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap()];
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selection = catalog.select_view_image_vision_model(&ContextConfig::default(), None, None);

    assert_eq!(
        selection.selected_mode,
        crate::types::ViewImageSelectedMode::NativeImageWithObservation
    );
    assert_eq!(selection.primary_provider.as_deref(), Some("anthropic"));
    assert_eq!(
        selection.primary_model.as_deref(),
        Some("claude-sonnet-4-6")
    );
    assert_eq!(selection.vision_provider.as_deref(), Some("anthropic"));
    assert_eq!(selection.vision_model.as_deref(), Some("claude-sonnet-4-6"));
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
fn view_image_vision_selection_skips_image_capable_unsupported_transports() {
    let mut fixture = test_app_config("gemini/gemini-2.5-pro", &[]);
    fixture.config.vision_candidate_models = vec![ModelRef::parse("openai/gpt-5.4-mini").unwrap()];
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
        ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
        ModelRef::parse("openai/gpt-5.4").unwrap(),
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
fn runtime_model_catalog_resolves_legacy_multi_endpoint_provider_identity() {
    let mut fixture = test_app_config("openai/gpt-5.4", &["volcengine/doubao-seedream-5.0-lite"]);
    let legacy_provider = ProviderId::parse("volcengine-agent").unwrap();
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
    assert_eq!(route.endpoint.endpoint.as_str(), "plan");
    assert_eq!(
        route.endpoint.runtime_config.id.as_str(),
        "volcengine-agent"
    );
}

#[test]
fn image_generation_config_defaults_to_auto() {
    let mut config = HolonConfigFile::default();

    assert_eq!(
        get_config_key(&config, "image_generation.default").unwrap(),
        json!("auto")
    );

    set_config_key(&mut config, "image_generation.default", "auto").unwrap();
    assert!(config.image_generation.default.is_none());
    assert!(config.image_generation.is_empty());
}

#[test]
fn image_generation_config_accepts_explicit_model_ref() {
    let mut config = HolonConfigFile::default();

    set_config_key(
        &mut config,
        "image_generation.default",
        "openai-codex/gpt-5.5",
    )
    .unwrap();

    assert_eq!(
        config.image_generation.default.as_deref(),
        Some("openai-codex/gpt-5.5")
    );
    assert_eq!(
        get_config_key(&config, "image_generation.default").unwrap(),
        json!("openai-codex/gpt-5.5")
    );

    unset_config_key(&mut config, "image_generation.default").unwrap();
    assert_eq!(
        get_config_key(&config, "image_generation.default").unwrap(),
        json!("auto")
    );
}

#[test]
fn generate_image_selection_auto_uses_turn_chain() {
    let fixture = test_app_config("arcee/trinity-mini", &["openai/gpt-image-2"]);
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(selected.as_string(), "openai/gpt-image-2");
}

#[test]
fn generate_image_selection_uses_explicit_image_generation_model() {
    let mut fixture = test_app_config("openai/gpt-image-2", &[]);
    fixture.config.image_generation_model = Some(ModelRef::parse("openai-codex/gpt-5.5").unwrap());
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(selected.as_string(), "openai-codex/gpt-5.5");
}

#[test]
fn generate_image_selection_requires_explicit_model_capability() {
    let mut fixture = test_app_config("openai/gpt-image-2", &[]);
    fixture.config.image_generation_model = Some(ModelRef::parse("openai/gpt-5.4").unwrap());
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    assert!(catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .is_none());
}

#[test]
fn generate_image_selection_accepts_volcengine_image_openai_seedream() {
    let mut fixture = test_app_config("openai/gpt-5.4", &[]);
    fixture.config.image_generation_model =
        Some(ModelRef::parse("volcengine-image-openai/doubao-seedream-5.0-lite").unwrap());
    fixture.config.providers.insert(
        ProviderId::parse("volcengine-image-openai").unwrap(),
        ProviderRuntimeConfig {
            id: ProviderId::parse("volcengine-image-openai").unwrap(),
            route_provider: ProviderId::parse("volcengine").unwrap(),
            route_endpoint: ProviderEndpointId::parse("plan").unwrap(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://ark.cn-beijing.volces.com/api/plan/v3".into(),
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

    assert_eq!(selected.as_string(), "volcengine/doubao-seedream-5.0-lite");
}

#[test]
fn generate_image_selection_accepts_canonical_volcengine_seedream() {
    let mut fixture = test_app_config("openai/gpt-5.4", &[]);
    fixture.config.image_generation_model =
        Some(ModelRef::parse("volcengine/doubao-seedream-5.0-lite").unwrap());
    let legacy_provider = ProviderId::parse("volcengine-agent").unwrap();
    let built_ins = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    fixture.config.providers.insert(
        legacy_provider.clone(),
        built_ins.get(&legacy_provider).unwrap().clone(),
    );
    let catalog = RuntimeModelCatalog::from_config(&fixture.config);

    let selected = catalog
        .select_generate_image_model(&ContextConfig::default(), None, None)
        .unwrap();

    assert_eq!(selected.as_string(), "volcengine/doubao-seedream-5.0-lite");
}

#[test]
fn runtime_model_catalog_resolves_canonical_seedream_route_endpoint() {
    let mut fixture = test_app_config("openai/gpt-5.4", &[]);
    fixture.config.image_generation_model =
        Some(ModelRef::parse("volcengine/doubao-seedream-5.0-lite").unwrap());
    let legacy_provider = ProviderId::parse("volcengine-agent").unwrap();
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
    assert_eq!(route.endpoint.endpoint.as_str(), "plan");
    assert_eq!(
        route.endpoint.runtime_config.id.as_str(),
        "volcengine-agent"
    );
}

#[test]
fn runtime_model_catalog_uses_discovery_cache_between_overrides_and_builtins() {
    let mut fixture = test_app_config("openrouter/anthropic/claude-3.5-sonnet", &[]);
    let remote_model_ref = ModelRef::parse("openrouter/anthropic/claude-3.5-sonnet").unwrap();
    fixture.config.model_discovery_cache.providers.insert(
        ProviderId::parse("openrouter").unwrap(),
        ProviderModelDiscoveryCache {
            provider: ProviderId::parse("openrouter").unwrap(),
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
}

#[test]
fn runtime_model_catalog_resolves_codex_spark_to_builtin_budget() {
    let fixture = test_app_config("openai-codex/gpt-5.3-codex-spark", &[]);
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
    assert_eq!(
        resolved.model_ref.as_string(),
        "openai-codex/gpt-5.3-codex-spark"
    );
    assert_eq!(resolved.prompt_budget_estimated_tokens, 121_600);
    assert_eq!(resolved.compaction_trigger_estimated_tokens, 109_440);
    assert_eq!(resolved.compaction_keep_recent_estimated_tokens, 41_587);
    assert_eq!(
        resolved.source,
        crate::model_catalog::ModelMetadataSource::BuiltInCatalog
    );
}

#[test]
fn invalid_model_override_percent_is_rejected() {
    let mut config = HolonConfigFile::default();
    let err = set_config_key(
        &mut config,
        "models.catalog",
        r#"{"anthropic/claude-sonnet-4-6":{"effective_context_window_percent":0}}"#,
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("effective_context_window_percent expects an integer from 1 to 100"));
}

#[test]
fn provider_doc_entries_are_sorted_and_populated() {
    let entries =
        built_in_provider_doc_entries().expect("built_in_provider_doc_entries should succeed");
    assert!(!entries.is_empty(), "should have at least one provider");
    // Legacy provider refs remain sorted for stable generated docs.
    for i in 1..entries.len() {
        assert!(
            entries[i - 1].id.as_str() <= entries[i].id.as_str(),
            "entries must be sorted by id"
        );
    }

    let dashscope_token_plan = entries
        .iter()
        .find(|entry| entry.legacy_provider.as_str() == "dashscope-token-plan")
        .expect("dashscope-token-plan doc entry");
    assert_eq!(dashscope_token_plan.id.as_str(), "dashscope-token-plan");
    assert_eq!(dashscope_token_plan.provider.as_str(), "dashscope");
    assert_eq!(dashscope_token_plan.endpoint.as_str(), "token-plan");

    let volcengine_agent = entries
        .iter()
        .find(|entry| entry.legacy_provider.as_str() == "volcengine-agent")
        .expect("volcengine-agent doc entry");
    assert_eq!(volcengine_agent.provider.as_str(), "volcengine");
    assert_eq!(volcengine_agent.endpoint.as_str(), "plan");
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
fn default_provider_ready_false_for_credential_source_none() {
    let mut fixture = test_app_config("openai/gpt-4o", &[]);
    // Simulate a local provider (e.g. vllm) with CredentialSource::None.
    let vllm = ProviderId::parse("vllm").unwrap();
    fixture.config.providers.insert(
        vllm.clone(),
        ProviderRuntimeConfig {
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
    fixture.config.default_model = ModelRef::parse("vllm/test-model").unwrap();
    assert!(
        !fixture.config.default_provider_ready(),
        "CredentialSource::None providers should not be considered ready"
    );
}

#[test]
fn default_provider_ready_false_when_provider_missing() {
    let mut fixture = test_app_config("openai/gpt-4o", &[]);
    fixture.config.default_model = ModelRef::parse("nonexistent/model").unwrap();
    assert!(
        !fixture.config.default_provider_ready(),
        "missing provider should not be ready"
    );
}
