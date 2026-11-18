use std::{collections::HashMap, env, path::Path, process::Command};

use holon_config::{
    built_in_provider_registry_with_settings, default_holon_home, get_config_value,
    resolve_anthropic_context_management_config, AnthropicCacheStrategy, CredentialKind,
    CredentialSource, ProviderId, ProviderTransportKind,
};

fn in_clean_environment(test: &str, assertions: impl FnOnce()) {
    const CHILD_TEST: &str = "HOLON_CONFIG_ENV_CHILD_TEST";
    if env::var(CHILD_TEST).as_deref() == Ok(test) {
        assertions();
        return;
    }

    // Config reads the process environment. Relaunch only this test with a
    // known environment so concurrent tests and developer credentials stay isolated.
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", test])
        .env_clear()
        .env(CHILD_TEST, test)
        .env("HOME", "/tmp/holon-home-test")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "{test} failed in a clean environment:\n{}\n{}",
        stdout,
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn config_falls_back_to_settings_values() {
    in_clean_environment("config_falls_back_to_settings_values", || {
        let mut settings = HashMap::new();
        settings.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            "https://example.com".to_string(),
        );

        let value = get_config_value("ANTHROPIC_BASE_URL", None, &settings);
        assert_eq!(value.as_deref(), Some("https://example.com"));
    });
}

#[test]
fn anthropic_runtime_cache_strategy_defaults_to_claude_code_prompt_cache() {
    in_clean_environment(
        "anthropic_runtime_cache_strategy_defaults_to_claude_code_prompt_cache",
        || {
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
        },
    );
}

#[test]
fn default_holon_home_uses_home_directory() {
    in_clean_environment("default_holon_home_uses_home_directory", || {
        assert_eq!(
            default_holon_home(),
            Path::new("/tmp/holon-home-test/.holon")
        );
    });
}

#[test]
fn built_in_provider_registry_includes_compatible_provider_defaults() {
    in_clean_environment(
        "built_in_provider_registry_includes_compatible_provider_defaults",
        || {
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

            let providers = built_in_provider_registry_with_settings(&settings_env).unwrap();

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
                built_in_provider_registry_with_settings(&volcengine_image_openai_only_env)
                    .unwrap();
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
        },
    );
}

#[test]
fn vercel_ai_gateway_falls_back_to_api_key_auth() {
    in_clean_environment("vercel_ai_gateway_falls_back_to_api_key_auth", || {
        let mut settings_env = HashMap::new();
        settings_env.insert("AI_GATEWAY_API_KEY".to_string(), "api-key".to_string());

        let providers = built_in_provider_registry_with_settings(&settings_env).unwrap();
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
    });
}
