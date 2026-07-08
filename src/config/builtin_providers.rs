use super::*;

pub fn validate_provider_config(
    provider_id: &ProviderId,
    provider_config: &ProviderConfigFile,
) -> Result<()> {
    parse_url_value("providers.<id>.base_url", &provider_config.base_url)?;
    validate_provider_auth(provider_id, &provider_config.auth)?;
    validate_provider_builtin_web_search(provider_id, provider_config)
}

pub(crate) fn persisted_provider_config_mut<'a>(
    config: &'a mut HolonConfigFile,
    key: &str,
    suffix: &str,
) -> Result<&'a mut ProviderConfigFile> {
    let rest = key
        .strip_prefix("providers.")
        .and_then(|value| value.strip_suffix(suffix))
        .ok_or_else(|| unknown_config_key(key))?;
    if rest.is_empty() {
        return Err(anyhow!(
            "providers.<id>{suffix} requires a non-empty provider id"
        ));
    }
    let id = ProviderId::parse(rest)?;
    if !config.providers.contains_key(&id) {
        let defaults = built_in_provider_default_config(&id)?
            .ok_or_else(|| anyhow!("provider {} not found", id.as_str()))?;
        config.providers.insert(id.clone(), defaults);
    }
    config
        .providers
        .get_mut(&id)
        .ok_or_else(|| anyhow!("provider {} not found", id.as_str()))
}

pub(crate) fn get_provider_config_key(config: &HolonConfigFile, key: &str) -> Result<Value> {
    let rest = key
        .strip_prefix("providers.")
        .ok_or_else(|| unknown_config_key(key))?;
    if rest.is_empty() {
        return Err(anyhow!("providers.<id> requires a non-empty provider id"));
    }
    if let Some(id) = rest.strip_suffix(".transport") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.transport.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".base_url") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.base_url.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.source") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.auth.source.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.kind") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.auth.kind.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.env") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.env.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.profile") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.profile.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.external") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.external.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if rest.contains('.') {
        return Err(unknown_config_key(key));
    }
    Ok(config
        .providers
        .get(&ProviderId::parse(rest)?)
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or(Value::Null))
}

pub(crate) fn set_provider_config_key(
    config: &mut HolonConfigFile,
    key: &str,
    raw_value: &str,
) -> Result<()> {
    if key.ends_with(".transport") {
        persisted_provider_config_mut(config, key, ".transport")?.transport =
            ProviderTransportKind::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".base_url") {
        let value = raw_value.trim();
        parse_url_value(key, value)?;
        persisted_provider_config_mut(config, key, ".base_url")?.base_url = value.to_string();
        return Ok(());
    }
    if key.ends_with(".auth.source") {
        persisted_provider_config_mut(config, key, ".auth.source")?
            .auth
            .source = CredentialSource::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".auth.kind") {
        persisted_provider_config_mut(config, key, ".auth.kind")?
            .auth
            .kind = CredentialKind::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".auth.env") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.env")?
            .auth
            .env = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    if key.ends_with(".auth.profile") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.profile")?
            .auth
            .profile = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    if key.ends_with(".auth.external") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.external")?
            .auth
            .external = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    Err(unknown_config_key(key))
}

pub(crate) fn unset_provider_config_key(config: &mut HolonConfigFile, key: &str) -> Result<()> {
    if key.ends_with(".auth.env") {
        persisted_provider_config_mut(config, key, ".auth.env")?
            .auth
            .env = None;
        return Ok(());
    }
    if key.ends_with(".auth.profile") {
        persisted_provider_config_mut(config, key, ".auth.profile")?
            .auth
            .profile = None;
        return Ok(());
    }
    if key.ends_with(".auth.external") {
        persisted_provider_config_mut(config, key, ".auth.external")?
            .auth
            .external = None;
        return Ok(());
    }
    let id = key
        .strip_prefix("providers.")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("providers.<id> requires a non-empty provider id"))?;
    if config.providers.remove(&ProviderId::parse(id)?).is_none() {
        return Err(anyhow!("provider {id} not found"));
    }
    Ok(())
}

pub fn built_in_provider_default_config(
    provider_id: &ProviderId,
) -> Result<Option<ProviderConfigFile>> {
    let settings_env = load_settings_env()?;
    built_in_provider_default_config_with_settings(provider_id, &settings_env)
}

pub(crate) fn built_in_provider_default_config_with_settings(
    provider_id: &ProviderId,
    settings_env: &HashMap<String, String>,
) -> Result<Option<ProviderConfigFile>> {
    let registry = built_in_provider_registry_with_settings(&settings_env)?;
    Ok(registry
        .get(provider_id)
        .map(|provider| ProviderConfigFile {
            transport: provider.transport,
            base_url: provider.base_url.clone(),
            auth: ProviderAuthConfig::default(),
            reasoning_effort: None,
            builtin_web_search: None,
        }))
}

/// Provider metadata for documentation generation.
#[derive(Debug, Clone)]
pub struct ProviderDocEntry {
    pub id: ProviderId,
    pub transport: ProviderTransportKind,
    pub base_url: String,
    pub auth_env: Option<String>,
}

/// Returns built-in provider metadata for documentation generation.
pub fn built_in_provider_doc_entries() -> Result<Vec<ProviderDocEntry>> {
    let settings_env = HashMap::new();
    let registry = built_in_provider_registry_with_settings(&settings_env)?;
    let mut entries: Vec<ProviderDocEntry> = registry
        .values()
        .map(|provider| ProviderDocEntry {
            id: provider.id.clone(),
            transport: provider.transport,
            base_url: provider.base_url.clone(),
            auth_env: provider.auth.env.clone(),
        })
        .collect();
    entries.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    Ok(entries)
}

pub fn provider_config_views(config: &AppConfig) -> Vec<ProviderConfigView> {
    config
        .providers
        .values()
        .map(|provider| provider_config_view(config, provider))
        .collect()
}

pub fn provider_config_view(
    config: &AppConfig,
    provider: &ProviderRuntimeConfig,
) -> ProviderConfigView {
    ProviderConfigView {
        id: provider.id.as_str().to_string(),
        transport: provider.transport.as_str().to_string(),
        base_url: provider.base_url.clone(),
        auth: ProviderAuthView {
            source: provider.auth.source.as_str().to_string(),
            kind: provider.auth.kind.as_str().to_string(),
            env: provider.auth.env.clone(),
            profile: provider.auth.profile.clone(),
            external: provider.auth.external.clone(),
        },
        credential_configured: provider.has_configured_credential()
            || matches!(provider.auth.source, CredentialSource::None),
        configured_in_config: config.stored_config.providers.contains_key(&provider.id),
    }
}

pub(crate) fn resolve_provider_registry(
    stored_config: &HolonConfigFile,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<ProviderRegistry> {
    let mut registry = built_in_provider_registry_with_settings(settings_env)?;
    for provider in registry.values_mut() {
        provider.credential = None;
    }
    for (id, provider_config) in &stored_config.providers {
        let built_in = registry.remove(id);
        let runtime = materialize_provider_config(
            id.clone(),
            provider_config.clone(),
            settings_env,
            credential_store,
            built_in,
        )?;
        registry.insert(id.clone(), runtime);
    }
    for provider in registry.values_mut() {
        if provider.credential.is_none() {
            provider.credential =
                resolve_provider_credential(&provider.auth, settings_env, credential_store)?;
        }
    }
    Ok(registry)
}

pub(crate) fn built_in_provider_registry_with_settings(
    settings_env: &HashMap<String, String>,
) -> Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::new();
    let openai_codex = ProviderId::openai_codex();
    let openai_codex_reasoning_effort = env::var("HOLON_OPENAI_CODEX_REASONING_EFFORT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some("low".to_string()))
        .map(|value| validate_openai_reasoning_effort(&value).map(|_| value))
        .transpose()?;
    registry.insert(
        openai_codex.clone(),
        ProviderRuntimeConfig {
            id: openai_codex,
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: env::var("HOLON_OPENAI_CODEX_BASE_URL")
                .unwrap_or_else(|_| "https://chatgpt.com/backend-api/codex".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(OPENAI_CODEX_CREDENTIAL_PROFILE.into()),
                external: Some("codex_cli".into()),
            },
            credential: None,
            credential_store_path: None,
            codex_home: Some(
                env::var("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| default_codex_home()),
            ),
            originator: Some("codex_cli_rs".into()),
            reasoning_effort: openai_codex_reasoning_effort,
            context_management: Default::default(),
            builtin_web_search: Some(openai_codex_builtin_web_search_config()),
        },
    );
    let openai = ProviderId::openai();
    registry.insert(
        openai.clone(),
        ProviderRuntimeConfig {
            id: openai,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: env::var("HOLON_OPENAI_BASE_URL")
                .ok()
                .or_else(|| env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENAI_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: env::var("OPENAI_API_KEY").ok(),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: Some(openai_builtin_web_search_config()),
        },
    );
    let anthropic = ProviderId::anthropic();
    registry.insert(
        anthropic.clone(),
        ProviderRuntimeConfig {
            id: anthropic,
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: get_config_value("ANTHROPIC_BASE_URL", None, settings_env)
                .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("ANTHROPIC_AUTH_TOKEN".into()),
                profile: None,
                external: None,
            },
            credential: get_config_value("ANTHROPIC_AUTH_TOKEN", None, settings_env),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: resolve_anthropic_context_management_config()?,
            builtin_web_search: Some(anthropic_builtin_web_search_config()),
        },
    );
    let gemini = ProviderId::gemini();
    registry.insert(
        gemini.clone(),
        ProviderRuntimeConfig {
            id: gemini,
            transport: ProviderTransportKind::GeminiGenerateContent,
            base_url: get_config_value("HOLON_GEMINI_BASE_URL", None, settings_env)
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("GEMINI_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: get_config_value("GEMINI_API_KEY", None, settings_env),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );
    insert_openai_compatible_provider(
        &mut registry,
        "arcee",
        "https://api.arcee.ai/api/v1",
        &["ARCEE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "byteplus",
        "https://ark.ap-southeast.bytepluses.com/api/v3",
        &["BYTEPLUS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "byteplus-coding",
        "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
        &["BYTEPLUS_CODING_API_KEY", "BYTEPLUS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "chutes",
        "https://llm.chutes.ai/v1",
        &["CHUTES_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "dashscope",
        "https://dashscope.aliyuncs.com/apps/anthropic",
        &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "dashscope-token-plan",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        &["DASHSCOPE_TOKEN_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "dashscope-coding-plan",
        "https://coding.dashscope.aliyuncs.com/apps/anthropic",
        &["DASHSCOPE_CODING_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "deepseek",
        "https://api.deepseek.com/anthropic",
        &["DEEPSEEK_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "fireworks",
        "https://api.fireworks.ai/inference/v1",
        &["FIREWORKS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "huggingface",
        "https://router.huggingface.co/v1",
        &["HUGGINGFACE_API_KEY", "HF_TOKEN"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "kilocode",
        "https://api.kilo.ai/api/gateway",
        &["KILOCODE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "litellm",
        "http://localhost:4000",
        &["LITELLM_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "mistral",
        "https://api.mistral.ai/v1",
        &["MISTRAL_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "moonshot",
        "https://api.moonshot.ai/v1",
        &["MOONSHOT_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "nearai",
        "https://cloud-api.near.ai/v1",
        &["NEARAI_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "nvidia",
        "https://integrate.api.nvidia.com/v1",
        &["NVIDIA_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "opencode-go",
        "https://opencode.ai/zen/go/v1",
        &["OPENCODE_GO_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "openrouter",
        "https://openrouter.ai/api/v1",
        &["OPENROUTER_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "qianfan",
        "https://qianfan.baidubce.com/v2",
        &["QIANFAN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "stepfun",
        "https://api.stepfun.ai/v1",
        &["STEPFUN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "stepfun-plan",
        "https://api.stepfun.ai/step_plan/v1",
        &["STEPFUN_PLAN_API_KEY", "STEPFUN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "synthetic",
        "https://api.synthetic.new/anthropic",
        &["SYNTHETIC_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "tencent-tokenhub",
        "https://tokenhub.tencentmaas.com/v1",
        &["TOKENHUB_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "together",
        "https://api.together.xyz/v1",
        &["TOGETHER_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "venice",
        "https://api.venice.ai/api/v1",
        &["VENICE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "vllm",
        "http://127.0.0.1:8000/v1",
        &[],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "volcengine",
        "https://ark.cn-beijing.volces.com/api/compatible",
        &["VOLCENGINE_API_KEY", "ARK_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "volcengine-coding",
        "https://ark.cn-beijing.volces.com/api/coding",
        &[
            "VOLCENGINE_CODING_API_KEY",
            "VOLCENGINE_API_KEY",
            "ARK_API_KEY",
        ],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "volcengine-agent",
        "https://ark.cn-beijing.volces.com/api/plan",
        &[
            "VOLCENGINE_AGENT_API_KEY",
            "VOLCENGINE_API_KEY",
            "ARK_API_KEY",
        ],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "volcengine-image-openai",
        "https://ark.cn-beijing.volces.com/api/plan/v3",
        &[
            "VOLCENGINE_IMAGE_OPENAI_API_KEY",
            "VOLCENGINE_AGENT_API_KEY",
            "VOLCENGINE_API_KEY",
            "ARK_API_KEY",
        ],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "xiaomi",
        "https://api.xiaomimimo.com/v1",
        &["XIAOMI_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "xiaomi-token-plan",
        "https://token-plan-cn.xiaomimimo.com/v1",
        &["XIAOMI_TOKEN_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        &mut registry,
        "xai",
        "https://api.x.ai/v1",
        &["XAI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "zai",
        "https://api.z.ai/api/anthropic",
        &["ZAI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "bigmodel",
        "https://open.bigmodel.cn/api/anthropic",
        &["BIGMODEL_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        &mut registry,
        "minimax",
        "https://api.minimax.io/anthropic",
        &["MINIMAX_API_KEY"],
        settings_env,
    )?;
    insert_vercel_ai_gateway_provider(&mut registry, settings_env)?;
    Ok(registry)
}

/// Public entry point for tools that need the built-in provider registry (e.g. docgen).
pub fn built_in_provider_registry() -> Result<ProviderRegistry> {
    let settings_env = load_settings_env()?;
    built_in_provider_registry_with_settings(&settings_env)
}

pub(crate) fn insert_openai_compatible_provider(
    registry: &mut ProviderRegistry,
    provider: &str,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    insert_builtin_http_provider(
        registry,
        provider,
        ProviderTransportKind::OpenAiChatCompletions,
        default_base_url,
        env_names,
        settings_env,
    )
}

/// Insert an Anthropic-compatible provider using Claude Code-like prompt-cache
/// lowering while avoiding implicit Claude-specific beta injection. Operators can
/// still override cache strategy and betas explicitly with env config.
pub(crate) fn insert_anthropic_compatible_provider(
    registry: &mut ProviderRegistry,
    provider: &str,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let builtin_web_search = match provider {
        "zai" => Some(zai_builtin_web_search_config()),
        "bigmodel" => Some(bigmodel_builtin_web_search_config()),
        "deepseek" => Some(deepseek_builtin_web_search_config()),
        _ => None,
    };
    insert_builtin_http_provider_with_context_management(
        registry,
        provider,
        ProviderTransportKind::AnthropicMessages,
        default_base_url,
        env_names,
        settings_env,
        resolve_anthropic_compatible_context_management_config()?,
        builtin_web_search,
    )
}

pub(crate) fn insert_vercel_ai_gateway_provider(
    registry: &mut ProviderRegistry,
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let context_management = resolve_anthropic_compatible_context_management_config()?;
    let oidc_credential = resolve_first_env_value(&["VERCEL_OIDC_TOKEN"], settings_env);
    let api_key_credential = resolve_first_env_value(
        &["AI_GATEWAY_API_KEY", "VERCEL_AI_GATEWAY_API_KEY"],
        settings_env,
    );
    let (kind, env_name, credential) = if let Some(credential) = oidc_credential {
        (
            CredentialKind::BearerToken,
            credential
                .env_name
                .clone()
                .unwrap_or_else(|| "VERCEL_OIDC_TOKEN".to_string()),
            Some(credential.value),
        )
    } else if let Some(credential) = api_key_credential {
        (
            CredentialKind::ApiKey,
            credential
                .env_name
                .clone()
                .unwrap_or_else(|| "AI_GATEWAY_API_KEY or VERCEL_AI_GATEWAY_API_KEY".to_string()),
            Some(credential.value),
        )
    } else {
        (
            CredentialKind::BearerToken,
            "VERCEL_OIDC_TOKEN or AI_GATEWAY_API_KEY or VERCEL_AI_GATEWAY_API_KEY".to_string(),
            None,
        )
    };
    let id = ProviderId::parse("vercel-ai-gateway")?;
    registry.insert(
        id.clone(),
        ProviderRuntimeConfig {
            id,
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: get_config_value("HOLON_VERCEL_AI_GATEWAY_BASE_URL", None, settings_env)
                .unwrap_or_else(|| "https://ai-gateway.vercel.sh".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind,
                env: Some(env_name),
                profile: None,
                external: None,
            },
            credential,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management,
            builtin_web_search: None,
        },
    );
    Ok(())
}

pub(crate) fn insert_builtin_http_provider(
    registry: &mut ProviderRegistry,
    provider: &str,
    transport: ProviderTransportKind,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    insert_builtin_http_provider_with_context_management(
        registry,
        provider,
        transport,
        default_base_url,
        env_names,
        settings_env,
        Default::default(),
        None,
    )
}

pub(crate) fn insert_builtin_http_provider_with_context_management(
    registry: &mut ProviderRegistry,
    provider: &str,
    transport: ProviderTransportKind,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
    context_management: AnthropicContextManagementConfig,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
) -> Result<()> {
    let id = ProviderId::parse(provider)?;
    let base_url_env = format!("HOLON_{}_BASE_URL", env_key_fragment(provider));
    let base_url = get_config_value(&base_url_env, None, settings_env)
        .unwrap_or_else(|| default_base_url.to_string());
    let credential = resolve_first_env_value(env_names, settings_env);
    let env_name = credential
        .as_ref()
        .and_then(|resolution| resolution.env_name.clone())
        .or_else(|| {
            if env_names.is_empty() {
                None
            } else {
                Some(env_names.join(" or "))
            }
        });
    registry.insert(
        id.clone(),
        ProviderRuntimeConfig {
            id,
            transport,
            base_url,
            auth: env_name
                .as_ref()
                .map(|env| ProviderAuthConfig {
                    source: CredentialSource::Env,
                    kind: CredentialKind::ApiKey,
                    env: Some(env.clone()),
                    profile: None,
                    external: None,
                })
                .unwrap_or_default(),
            credential: credential.map(|resolution| resolution.value),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management,
            builtin_web_search,
        },
    );
    Ok(())
}

pub(crate) fn openai_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::OpenAi,
        advertised_tool_type: "web_search_preview".to_string(),
        backend_kind: "openai_web_search".to_string(),
    }
}

pub(crate) fn openai_codex_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::OpenAi,
        advertised_tool_type: "web_search".to_string(),
        backend_kind: "openai_codex_web_search".to_string(),
    }
}

pub(crate) fn anthropic_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "anthropic_web_search".to_string(),
    }
}

pub(crate) fn zai_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "zai_web_search_prime".to_string(),
    }
}

pub(crate) fn bigmodel_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "bigmodel_web_search".to_string(),
    }
}

pub(crate) fn deepseek_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "deepseek_web_search".to_string(),
    }
}

pub(crate) fn env_key_fragment(provider: &str) -> String {
    provider
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn resolve_first_env_value(
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Option<ResolvedEnvValue> {
    env_names.iter().find_map(|env_name| {
        get_config_value(env_name, None, settings_env).map(|value| ResolvedEnvValue {
            env_name: Some((*env_name).to_string()),
            value,
        })
    })
}

pub(crate) struct ResolvedEnvValue {
    env_name: Option<String>,
    value: String,
}

pub(crate) fn materialize_provider_config(
    id: ProviderId,
    provider_config: ProviderConfigFile,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
    built_in: Option<ProviderRuntimeConfig>,
) -> Result<ProviderRuntimeConfig> {
    validate_provider_auth(&id, &provider_config.auth)?;
    let credential =
        resolve_provider_credential(&provider_config.auth, settings_env, credential_store)?;
    let mut runtime = built_in.unwrap_or_else(|| ProviderRuntimeConfig {
        id: id.clone(),
        transport: provider_config.transport,
        base_url: provider_config.base_url.clone(),
        auth: provider_config.auth.clone(),
        credential: None,
        credential_store_path: None,
        codex_home: None,
        originator: None,
        reasoning_effort: None,
        context_management: Default::default(),
        builtin_web_search: None,
    });
    if let Some(reasoning_effort) = provider_config.reasoning_effort.as_deref() {
        validate_openai_reasoning_effort(reasoning_effort)?;
    }
    validate_provider_builtin_web_search(&id, &provider_config)?;
    runtime.id = id;
    runtime.transport = provider_config.transport;
    runtime.base_url = provider_config.base_url;
    runtime.auth = provider_config.auth;
    runtime.credential = credential;
    if provider_config.reasoning_effort.is_some() {
        runtime.reasoning_effort = provider_config.reasoning_effort;
    }
    if let Some(builtin_web_search) = provider_config.builtin_web_search {
        runtime.builtin_web_search = builtin_web_search.enabled.then_some(builtin_web_search);
    }
    Ok(runtime)
}

pub(crate) fn validate_openai_reasoning_effort(value: &str) -> Result<()> {
    match value {
        "low" | "medium" | "high" | "xhigh" => Ok(()),
        _ => Err(anyhow!(
            "invalid OpenAI Codex reasoning_effort '{value}'; must be one of low, medium, high, xhigh"
        )),
    }
}

pub(crate) fn validate_provider_builtin_web_search(
    provider_id: &ProviderId,
    provider_config: &ProviderConfigFile,
) -> Result<()> {
    let Some(search) = provider_config.builtin_web_search.as_ref() else {
        return Ok(());
    };
    if !search.enabled {
        return Ok(());
    }
    if search.advertised_tool_type.trim().is_empty() {
        return Err(anyhow!(
            "providers.{}.builtin_web_search.advertised_tool_type must not be empty",
            provider_id.as_str()
        ));
    }
    if search.backend_kind.trim().is_empty() {
        return Err(anyhow!(
            "providers.{}.builtin_web_search.backend_kind must not be empty",
            provider_id.as_str()
        ));
    }
    match (provider_config.transport, search.kind) {
        (ProviderTransportKind::OpenAiResponses, ProviderNativeWebSearchKind::OpenAi) => {
            if search.advertised_tool_type == "web_search_preview" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be web_search_preview for OpenAI Responses native search",
                    provider_id.as_str()
                ))
            }
        }
        (ProviderTransportKind::OpenAiCodexResponses, ProviderNativeWebSearchKind::OpenAi) => {
            if search.advertised_tool_type == "web_search" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be web_search for OpenAI Codex Responses native search",
                    provider_id.as_str()
                ))
            }
        }
        (ProviderTransportKind::AnthropicMessages, ProviderNativeWebSearchKind::Anthropic) => {
            if search.advertised_tool_type == "web_search_20250305" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be web_search_20250305 for Anthropic Messages native search",
                    provider_id.as_str()
                ))
            }
        }
        (ProviderTransportKind::GeminiGenerateContent, ProviderNativeWebSearchKind::Gemini) => {
            if search.advertised_tool_type == "google_search" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be google_search for Gemini native search",
                    provider_id.as_str()
                ))
            }
        }
        _ => Err(anyhow!(
            "providers.{}.builtin_web_search kind {:?} is incompatible with transport {:?}",
            provider_id.as_str(),
            search.kind,
            provider_config.transport
        )),
    }
}

pub(crate) fn resolve_provider_credential(
    auth: &ProviderAuthConfig,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<Option<String>> {
    match auth.source {
        CredentialSource::Env => Ok(auth
            .env
            .as_deref()
            .and_then(|key| get_config_value(key, None, settings_env))),
        CredentialSource::AuthProfile => auth
            .profile
            .as_deref()
            .map(normalize_credential_profile_id)
            .transpose()?
            .and_then(|profile| credential_store.profiles.get(&profile))
            .map(|entry| {
                if entry.kind != auth.kind {
                    return Err(anyhow!(
                        "credential profile {} has kind {}, but provider expects {}",
                        auth.profile.as_deref().unwrap_or_default(),
                        entry.kind.as_str(),
                        auth.kind.as_str()
                    ));
                }
                Ok(entry.material.clone())
            })
            .transpose(),
        CredentialSource::None
        | CredentialSource::ExternalCli
        | CredentialSource::CredentialProcess => Ok(None),
    }
}

pub(crate) fn validate_provider_auth(
    provider_id: &ProviderId,
    auth: &ProviderAuthConfig,
) -> Result<()> {
    match (auth.source, auth.kind) {
        (CredentialSource::Env, CredentialKind::ApiKey | CredentialKind::BearerToken) => {
            if auth.env.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(anyhow!(
                    "provider {} env auth requires auth.env",
                    provider_id.as_str()
                ));
            }
        }
        (CredentialSource::ExternalCli, CredentialKind::SessionToken) => {
            if auth
                .external
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(anyhow!(
                    "provider {} external_cli auth requires auth.external",
                    provider_id.as_str()
                ));
            }
        }
        (
            CredentialSource::AuthProfile,
            CredentialKind::ApiKey
            | CredentialKind::BearerToken
            | CredentialKind::OAuth
            | CredentialKind::SessionToken,
        ) => {
            let profile = auth.profile.as_deref().ok_or_else(|| {
                anyhow!(
                    "provider {} credential_profile auth requires auth.profile",
                    provider_id.as_str()
                )
            })?;
            if profile.trim().is_empty() {
                return Err(anyhow!(
                    "provider {} credential_profile auth requires auth.profile",
                    provider_id.as_str()
                ));
            }
            normalize_credential_profile_id(profile).with_context(|| {
                format!(
                    "provider {} credential_profile auth has invalid auth.profile",
                    provider_id.as_str()
                )
            })?;
        }
        (CredentialSource::None, CredentialKind::None) => {}
        _ => {
            return Err(anyhow!(
                "provider {} unsupported auth contract {}+{}",
                provider_id.as_str(),
                auth.source.as_str(),
                auth.kind.as_str()
            ));
        }
    }
    Ok(())
}

pub fn provider_registry_for_tests(
    openai_key: Option<&str>,
    anthropic_token: Option<&str>,
    codex_home: PathBuf,
) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    let openai_codex = ProviderId::openai_codex();
    registry.insert(
        openai_codex.clone(),
        ProviderRuntimeConfig {
            id: openai_codex,
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(OPENAI_CODEX_CREDENTIAL_PROFILE.into()),
                external: Some("codex_cli".into()),
            },
            credential: None,
            credential_store_path: None,
            codex_home: Some(codex_home),
            originator: Some("codex_cli_rs".into()),
            reasoning_effort: Some("low".into()),
            context_management: Default::default(),
            builtin_web_search: Some(openai_codex_builtin_web_search_config()),
        },
    );
    let openai = ProviderId::openai();
    registry.insert(
        openai.clone(),
        ProviderRuntimeConfig {
            id: openai,
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENAI_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: openai_key.map(ToString::to_string),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: Some(openai_builtin_web_search_config()),
        },
    );
    let anthropic = ProviderId::anthropic();
    registry.insert(
        anthropic.clone(),
        ProviderRuntimeConfig {
            id: anthropic,
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: "https://api.anthropic.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("ANTHROPIC_AUTH_TOKEN".into()),
                profile: None,
                external: None,
            },
            credential: anthropic_token.map(ToString::to_string),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: Some(anthropic_builtin_web_search_config()),
        },
    );
    let gemini = ProviderId::gemini();
    registry.insert(
        gemini.clone(),
        ProviderRuntimeConfig {
            id: gemini,
            transport: ProviderTransportKind::GeminiGenerateContent,
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("GEMINI_API_KEY".into()),
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
    registry
}
