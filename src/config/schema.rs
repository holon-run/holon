use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct ConfigSchemaEntry {
    pub key: &'static str,
    pub kind: &'static str,
    pub description: &'static str,
    pub default: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<&'static str>,
}

pub fn config_schema() -> Vec<ConfigSchemaEntry> {
    vec![
        ConfigSchemaEntry {
            key: "api.cors.enabled",
            kind: "boolean",
            description: "Enable CORS responses on the HTTP/control API. Enabled by default for localhost/loopback browser origins.",
            default: json!(true),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "api.cors.allowed_origins",
            kind: "string_list",
            description: "Additional browser origins for HTTP/control API CORS. Localhost/loopback origins are allowed by default; wildcard is rejected when credentials are enabled.",
            default: json!([]),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "api.cors.allowed_methods",
            kind: "string_list",
            description: "HTTP methods allowed by CORS preflight responses.",
            default: json!(default_api_cors_allowed_methods()),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "api.cors.allowed_headers",
            kind: "string_list",
            description: "HTTP request headers allowed by CORS preflight responses.",
            default: json!(default_api_cors_allowed_headers()),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "api.cors.allow_credentials",
            kind: "boolean",
            description: "Allow credentialed browser CORS requests. Cannot be combined with wildcard origins.",
            default: json!(false),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "api.cors.max_age_seconds",
            kind: "positive_integer",
            description: "Seconds browsers may cache successful CORS preflight responses.",
            default: json!(600),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.default",
            kind: "model_ref",
            description: "Explicit default provider/model ref. When unset, the runtime derives one from authenticated providers.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.fallbacks",
            kind: "model_ref_list",
            description:
                "Explicit fallback provider/model refs. Null or an empty persisted list means unset; when unset, the runtime derives fallbacks from authenticated providers.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "vision.default",
            kind: "model_ref",
            description:
                "Explicit provider/model ref for ViewImage visual observation generation. When unset, ViewImage auto-discovers an authenticated image-capable provider and keeps model.fallbacks only as a compatibility candidate source.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "image_generation.default",
            kind: "model_ref_or_auto",
            description:
                "Provider/model ref for GenerateImage. The default auto mode selects the first configured turn model that supports image_generation.",
            default: json!("auto"),
            allowed_values: vec!["auto"],
        },
        ConfigSchemaEntry {
            key: "providers.<id>.transport",
            kind: "enum",
            description: "Model provider transport used for the provider account/profile.",
            default: Value::Null,
            allowed_values: vec![
                "openai_codex_responses",
                "openai_responses",
                "openai_chat_completions",
                "anthropic_messages",
                "gemini_generate_content",
            ],
        },
        ConfigSchemaEntry {
            key: "providers.<id>.base_url",
            kind: "string",
            description: "Model provider API base URL.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "providers.<id>.auth.source",
            kind: "enum",
            description: "Credential source for the provider account/profile.",
            default: Value::Null,
            allowed_values: vec![
                "env",
                "external_cli",
                "credential_profile",
                "credential_process",
                "none",
            ],
        },
        ConfigSchemaEntry {
            key: "providers.<id>.auth.kind",
            kind: "enum",
            description: "Credential material kind for the provider account/profile.",
            default: Value::Null,
            allowed_values: vec![
                "api_key",
                "bearer_token",
                "oauth",
                "session_token",
                "aws_sdk",
                "none",
            ],
        },
        ConfigSchemaEntry {
            key: "providers.<id>.auth.env",
            kind: "string",
            description: "Environment variable name used when auth.source=env.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "providers.<id>.auth.profile",
            kind: "string",
            description: "Credential profile id used when auth.source=credential_profile.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "providers.<id>.auth.external",
            kind: "string",
            description: "External credential provider id used when auth.source=external_cli.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "models.catalog",
            kind: "json_object",
            description: "Per-model runtime metadata and policy keyed by provider/model ref.",
            default: json!({}),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback",
            kind: "json_object",
            description: "Explicit runtime policy fallback used for unknown models.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.context_window_tokens",
            kind: "positive_integer",
            description: "Optional fallback context window for unknown models.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.effective_context_window_percent",
            kind: "percentage_integer",
            description: "Optional usable-context percent for unknown-model fallback.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.prompt_budget_estimated_tokens",
            kind: "positive_integer",
            description: "Fallback prompt budget used when model metadata is unknown.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.compaction_trigger_estimated_tokens",
            kind: "positive_integer",
            description: "Fallback compaction trigger used when model metadata is unknown.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.compaction_keep_recent_estimated_tokens",
            kind: "positive_integer",
            description: "Fallback uncompacted recent-context budget for unknown models.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "model.unknown_fallback.runtime_max_output_tokens",
            kind: "positive_integer",
            description: "Fallback max output token budget for unknown models.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.max_output_tokens",
            kind: "positive_integer",
            description: "Default max output token budget for providers.",
            default: json!(8192),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.default_tool_output_tokens",
            kind: "positive_integer",
            description: "Default model-visible output token budget for local command tools.",
            default: json!(crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.max_tool_output_tokens",
            kind: "positive_integer",
            description: "Upper model-visible output token budget for local command tools.",
            default: json!(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.prompt_budget_estimated_tokens",
            kind: "positive_integer",
            description: "Estimated token budget for one assembled context projection.",
            default: json!(4096),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.compaction_trigger_estimated_tokens",
            kind: "positive_integer",
            description: "Estimated visible-token threshold that triggers legacy message compaction fallback.",
            default: json!(2048),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.compaction_keep_recent_estimated_tokens",
            kind: "positive_integer",
            description: "Estimated visible-token budget kept un-compacted in the legacy message fallback.",
            default: json!(768),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.recent_episode_candidates",
            kind: "positive_integer",
            description: "Number of archived episodes considered for relevance ranking during prompt assembly.",
            default: json!(12),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.max_relevant_episodes",
            kind: "positive_integer",
            description: "Maximum number of archived episodes rendered into prompt context.",
            default: json!(3),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "runtime.disable_provider_fallback",
            kind: "boolean",
            description: "Disable provider/model fallback and require deterministic single-provider execution.",
            default: json!(false),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "tui.alternate_screen",
            kind: "enum",
            description: "Whether the TUI uses the terminal alternate screen buffer.",
            default: json!("auto"),
            allowed_values: vec!["auto", "always", "never"],
        },
        ConfigSchemaEntry {
            key: "web.fetch.enabled",
            kind: "boolean",
            description: "Enable the runtime-native WebFetch tool.",
            default: json!(true),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "web.fetch.max_chars",
            kind: "positive_integer",
            description: "Maximum model-visible characters returned by WebFetch.",
            default: json!(crate::web::WebFetchConfig::default().max_chars),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.max_response_bytes",
            kind: "positive_integer",
            description: "Maximum response bytes read by WebFetch before truncation.",
            default: json!(crate::web::WebFetchConfig::default().max_response_bytes),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.timeout_seconds",
            kind: "positive_integer",
            description: "Per-request timeout for WebFetch and managed WebSearch providers.",
            default: json!(crate::web::WebFetchConfig::default().timeout_seconds),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.max_redirects",
            kind: "positive_integer",
            description: "Maximum redirect hops followed by WebFetch.",
            default: json!(crate::web::WebFetchConfig::default().max_redirects),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.allowed_hosts",
            kind: "string_list",
            description: "Hosts or host:port entries allowed by WebFetch, including explicit dev loopback exceptions.",
            default: json!([]),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.fetch.denied_hosts",
            kind: "string_list",
            description: "Hosts or host:port entries blocked by WebFetch.",
            default: json!([]),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.search.enabled",
            kind: "boolean",
            description: "Enable the WebSearch provider-routed tool.",
            default: json!(true),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "web.search.builtin_provider.enabled",
            kind: "boolean",
            description: "Enable provider-declared builtin web search by default when the active model provider supports it.",
            default: json!(true),
            allowed_values: vec!["true", "false"],
        },
        ConfigSchemaEntry {
            key: "web.search.provider",
            kind: "string",
            description: "Default WebSearch provider id, or auto for configured routing policy.",
            default: json!("auto"),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.search.mode",
            kind: "enum",
            description: "WebSearch routing mode: single, fallback, or aggregate.",
            default: json!("fallback"),
            allowed_values: vec!["single", "fallback", "aggregate"],
        },
        ConfigSchemaEntry {
            key: "web.search.providers",
            kind: "string_list",
            description: "Explicit WebSearch provider attempt order for auto fallback or aggregate mode.",
            default: json!([]),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.search.max_results",
            kind: "positive_integer",
            description: "Maximum number of WebSearch results returned to the model.",
            default: json!(crate::web::WebSearchConfig::default().max_results),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.search.max_provider_attempts",
            kind: "positive_integer",
            description: "Maximum WebSearch providers attempted for fallback or aggregate routing.",
            default: json!(crate::web::WebSearchConfig::default().max_provider_attempts),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.kind",
            kind: "string",
            description: "Web search provider kind: duck_duck_go, searxng, brave, tencent_cloud_wsa, bocha, tavily, exa, perplexity, firecrawl, open_ai_native, anthropic_native, gemini_native, command.",
            default: Value::Null,
            allowed_values: vec!["duck_duck_go", "searxng", "brave", "tencent_cloud_wsa", "bocha", "tavily", "exa", "perplexity", "firecrawl", "open_ai_native", "anthropic_native", "gemini_native", "command"],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.base_url",
            kind: "string",
            description: "Optional custom base URL for the web search provider.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.capabilities",
            kind: "json_object",
            description: "Derived WebSearch provider capability metadata used for routing diagnostics.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.credential_profile",
            kind: "string",
            description: "Named credential profile to load the API key from. The profile must be of kind api_key.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.command.argv",
            kind: "string_list",
            description: "Fixed command argv template for kind=command WebSearch providers. Supports {{query}} and {{max_results}} substitutions.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.output.format",
            kind: "enum",
            description: "Command provider stdout format.",
            default: json!("json"),
            allowed_values: vec!["json"],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.output.mapping.title",
            kind: "string",
            description: "JSON path used to map each command result title.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.output.mapping.url",
            kind: "string",
            description: "JSON path used to map each command result URL.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.output.mapping.snippet",
            kind: "string",
            description: "Optional JSON path used to map each command result snippet.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.output.mapping.published_at",
            kind: "string",
            description: "Optional JSON path used to map each command result publication timestamp.",
            default: Value::Null,
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.limits.timeout_ms",
            kind: "positive_integer",
            description: "Command provider execution timeout in milliseconds.",
            default: json!(crate::web::WebProviderLimitsConfig::default().timeout_ms),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "web.providers.<name>.limits.max_output_bytes",
            kind: "positive_integer",
            description: "Command provider stdout byte limit.",
            default: json!(crate::web::WebProviderLimitsConfig::default().max_output_bytes),
            allowed_values: vec![],
        },
        ConfigSchemaEntry {
            key: "agent_templates.remote_sources",
            kind: "json_object",
            description: "Remote AgentTemplate sources keyed by source id. Each source supports url, ref, enabled, and optional credential_profile. The credential profile must contain an api_key or bearer_token GitHub token.",
            default: json!({}),
            allowed_values: vec![],
        },
    ]
}

pub fn get_config_key(config: &HolonConfigFile, key: &str) -> Result<Value> {
    match key {
        "api.cors.enabled" => Ok(config
            .api
            .cors
            .enabled
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "api.cors.allowed_origins" => Ok(json!(config.api.cors.allowed_origins)),
        "api.cors.allowed_methods" => Ok(json!(config.api.cors.allowed_methods)),
        "api.cors.allowed_headers" => Ok(json!(config.api.cors.allowed_headers)),
        "api.cors.allow_credentials" => Ok(config
            .api
            .cors
            .allow_credentials
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "api.cors.max_age_seconds" => Ok(config
            .api
            .cors
            .max_age_seconds
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.default" => Ok(config
            .model
            .default
            .as_ref()
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null)),
        "model.fallbacks" => Ok(Value::Array(
            config
                .model
                .fallbacks
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        )),
        "vision.default" => Ok(config
            .vision
            .default
            .as_ref()
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null)),
        "image_generation.default" => Ok(config
            .image_generation
            .default
            .as_ref()
            .map(|value| Value::String(value.clone()))
            .unwrap_or_else(|| json!("auto"))),
        "models.catalog" => Ok(serde_json::to_value(&config.models.catalog)?),
        "model.unknown_fallback" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.context_window_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.context_window_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.effective_context_window_percent" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.effective_context_window_percent)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.prompt_budget_estimated_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.prompt_budget_estimated_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.compaction_trigger_estimated_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.compaction_trigger_estimated_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.compaction_keep_recent_estimated_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.compaction_keep_recent_estimated_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "model.unknown_fallback.runtime_max_output_tokens" => Ok(config
            .model
            .unknown_fallback
            .as_ref()
            .and_then(|value| value.runtime_max_output_tokens)
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "providers" => Ok(serde_json::to_value(&config.providers)?),
        key if key.starts_with("providers.") => get_provider_config_key(config, key),
        "runtime.max_output_tokens" => Ok(config
            .runtime
            .max_output_tokens
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "runtime.default_tool_output_tokens" => Ok(config
            .runtime
            .default_tool_output_tokens
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "runtime.max_tool_output_tokens" => Ok(config
            .runtime
            .max_tool_output_tokens
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "runtime.disable_provider_fallback" => Ok(config
            .runtime
            .disable_provider_fallback
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "tui.alternate_screen" => Ok(config
            .tui
            .alternate_screen
            .map(|value| Value::String(value.as_str().to_string()))
            .unwrap_or(Value::Null)),
        "web.fetch.enabled" => Ok(config
            .web
            .fetch
            .enabled
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "web.fetch.max_chars" => Ok(config
            .web
            .fetch
            .max_chars
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.fetch.max_response_bytes" => Ok(config
            .web
            .fetch
            .max_response_bytes
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.fetch.timeout_seconds" => Ok(config
            .web
            .fetch
            .timeout_seconds
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.fetch.max_redirects" => Ok(config
            .web
            .fetch
            .max_redirects
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.fetch.allowed_hosts" => Ok(json!(config.web.fetch.allowed_hosts)),
        "web.fetch.denied_hosts" => Ok(json!(config.web.fetch.denied_hosts)),
        "web.search.enabled" => Ok(config
            .web
            .search
            .enabled
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "web.search.builtin_provider.enabled" => Ok(config
            .web
            .search
            .builtin_provider
            .enabled
            .map(Value::Bool)
            .unwrap_or(Value::Null)),
        "web.search.provider" => Ok(config
            .web
            .search
            .provider
            .as_ref()
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null)),
        "web.search.mode" => Ok(config
            .web
            .search
            .mode
            .map(|value| Value::String(value.as_str().to_string()))
            .unwrap_or(Value::Null)),
        "web.search.providers" => Ok(json!(config.web.search.providers)),
        "web.search.max_results" => Ok(config
            .web
            .search
            .max_results
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.search.max_provider_attempts" => Ok(config
            .web
            .search
            .max_provider_attempts
            .map(|value| json!(value))
            .unwrap_or(Value::Null)),
        "web.providers" => Ok(serde_json::to_value(&config.web.providers)?),
        key if key.starts_with("web.providers.") => {
            let name = key.strip_prefix("web.providers.").unwrap();
            if let Some(provider_name) = name.strip_suffix(".kind") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .map(|p| Value::String(p.kind.as_str().to_string()))
                    .unwrap_or(Value::Null));
            }
            if let Some(provider_name) = name.strip_suffix(".base_url") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.base_url.as_ref())
                    .map(|v| Value::String(v.clone()))
                    .unwrap_or(Value::Null));
            }
            if let Some(provider_name) = name.strip_suffix(".capabilities") {
                return match config.web.providers.get(provider_name) {
                    Some(provider) => Ok(serde_json::to_value(provider.kind.capabilities())?),
                    None => Ok(Value::Null),
                };
            }
            if let Some(provider_name) = name.strip_suffix(".credential_profile") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.credential_profile.as_ref())
                    .map(|v| Value::String(v.clone()))
                    .unwrap_or(Value::Null));
            }
            if let Some(provider_name) = name.strip_suffix(".command.argv") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.command.as_ref())
                    .map(|command| json!(command.argv))
                    .unwrap_or(Value::Null));
            }
            if let Some(provider_name) = name.strip_suffix(".output.format") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.output.as_ref())
                    .map(|output| json!(output.format))
                    .unwrap_or(Value::Null));
            }
            if let Some((provider_name, field)) = web_provider_output_mapping_key(name) {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.output.as_ref())
                    .and_then(|output| output_mapping_field(&output.mapping, field))
                    .map(|value| Value::String(value.to_string()))
                    .unwrap_or(Value::Null));
            }
            if let Some(provider_name) = name.strip_suffix(".limits.timeout_ms") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.limits.timeout_ms)
                    .map(|value| json!(value))
                    .unwrap_or(Value::Null));
            }
            if let Some(provider_name) = name.strip_suffix(".limits.max_output_bytes") {
                return Ok(config
                    .web
                    .providers
                    .get(provider_name)
                    .and_then(|p| p.limits.max_output_bytes)
                    .map(|value| json!(value))
                    .unwrap_or(Value::Null));
            }
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name> requires a non-empty provider name"
                ));
            }
            match config.web.providers.get(name) {
                Some(provider) => Ok(serde_json::to_value(provider)?),
                None => Ok(Value::Null),
            }
        }
        _ => Err(unknown_config_key(key)),
    }
}

pub fn set_config_key(config: &mut HolonConfigFile, key: &str, raw_value: &str) -> Result<()> {
    match key {
        "api.cors.enabled" => {
            config.api.cors.enabled = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "api.cors.allowed_origins" => {
            config.api.cors.allowed_origins = parse_string_list(raw_value)?;
        }
        "api.cors.allowed_methods" => {
            config.api.cors.allowed_methods = parse_string_list(raw_value)?;
        }
        "api.cors.allowed_headers" => {
            config.api.cors.allowed_headers = parse_string_list(raw_value)?;
        }
        "api.cors.allow_credentials" => {
            config.api.cors.allow_credentials = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "api.cors.max_age_seconds" => {
            config.api.cors.max_age_seconds = Some(parse_positive_u64_key(key, raw_value)?);
        }
        "model.default" => {
            let parsed = ModelRef::parse(raw_value)?;
            config.model.default = Some(parsed.as_string());
        }
        "model.fallbacks" => {
            config.model.fallbacks = parse_model_ref_list(raw_value)?
                .into_iter()
                .map(|model| model.as_string())
                .collect();
        }
        "vision.default" => {
            let parsed = ModelRef::parse(raw_value)?;
            config.vision.default = Some(parsed.as_string());
        }
        "image_generation.default" => {
            if raw_value.trim().eq_ignore_ascii_case("auto") {
                config.image_generation.default = None;
            } else {
                let parsed = ModelRef::parse(raw_value)?;
                config.image_generation.default = Some(parsed.as_string());
            }
        }
        "models.catalog" => {
            config.models.catalog = parse_model_catalog_value(raw_value)?;
        }
        "model.unknown_fallback" => {
            config.model.unknown_fallback = parse_optional_model_runtime_override(raw_value)?;
        }
        "model.unknown_fallback.context_window_tokens" => {
            ensure_unknown_model_fallback(config).context_window_tokens =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        "model.unknown_fallback.effective_context_window_percent" => {
            ensure_unknown_model_fallback(config).effective_context_window_percent =
                Some(parse_percentage_u8_key(key, raw_value)?);
        }
        "model.unknown_fallback.prompt_budget_estimated_tokens" => {
            ensure_unknown_model_fallback(config).prompt_budget_estimated_tokens =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        "model.unknown_fallback.compaction_trigger_estimated_tokens" => {
            ensure_unknown_model_fallback(config).compaction_trigger_estimated_tokens =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        "model.unknown_fallback.compaction_keep_recent_estimated_tokens" => {
            ensure_unknown_model_fallback(config).compaction_keep_recent_estimated_tokens =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        "model.unknown_fallback.runtime_max_output_tokens" => {
            ensure_unknown_model_fallback(config).runtime_max_output_tokens =
                Some(parse_positive_u32_key(key, raw_value)?);
        }
        key if key.starts_with("providers.") => set_provider_config_key(config, key, raw_value)?,
        "runtime.max_output_tokens" => {
            let value = parse_positive_u32_key(key, raw_value)?;
            config.runtime.max_output_tokens = Some(value);
        }
        "runtime.default_tool_output_tokens" => {
            config.runtime.default_tool_output_tokens = Some(
                parse_positive_u32_key(key, raw_value)?
                    .min(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32),
            );
        }
        "runtime.max_tool_output_tokens" => {
            config.runtime.max_tool_output_tokens = Some(
                parse_positive_u32_key(key, raw_value)?
                    .min(crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32),
            );
        }
        "runtime.disable_provider_fallback" => {
            config.runtime.disable_provider_fallback = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "tui.alternate_screen" => {
            config.tui.alternate_screen = Some(AltScreenMode::parse(raw_value)?);
        }
        "web.fetch.enabled" => {
            config.web.fetch.enabled = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "web.fetch.max_chars" => {
            config.web.fetch.max_chars = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "web.fetch.max_response_bytes" => {
            config.web.fetch.max_response_bytes = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "web.fetch.timeout_seconds" => {
            config.web.fetch.timeout_seconds = Some(parse_positive_u64_key(key, raw_value)?);
        }
        "web.fetch.max_redirects" => {
            config.web.fetch.max_redirects = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "web.fetch.allowed_hosts" => {
            config.web.fetch.allowed_hosts = parse_string_list(raw_value)?;
        }
        "web.fetch.denied_hosts" => {
            config.web.fetch.denied_hosts = parse_string_list(raw_value)?;
        }
        "web.search.enabled" => {
            config.web.search.enabled = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "web.search.builtin_provider.enabled" => {
            config.web.search.builtin_provider.enabled = Some(
                parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?,
            );
        }
        "web.search.provider" => {
            let provider = raw_value.trim();
            if provider.is_empty() {
                return Err(anyhow!("{key} expects a non-empty provider id"));
            }
            config.web.search.provider = Some(provider.to_string());
        }
        "web.search.mode" => {
            config.web.search.mode = Some(
                serde_json::from_value(serde_json::Value::String(raw_value.trim().to_string()))
                    .with_context(|| format!("invalid web search mode: {}", raw_value))?,
            );
        }
        "web.search.providers" => {
            config.web.search.providers = parse_string_list(raw_value)?;
        }
        "web.search.max_results" => {
            config.web.search.max_results = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "web.search.max_provider_attempts" => {
            config.web.search.max_provider_attempts =
                Some(parse_positive_usize_key(key, raw_value)?);
        }
        key if key.starts_with("web.providers.") && key.ends_with(".kind") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".kind").unwrap();
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name>.kind requires a non-empty provider name"
                ));
            }
            let kind: WebProviderKind =
                serde_json::from_value(serde_json::Value::String(raw_value.trim().to_string()))
                    .with_context(|| format!("invalid web provider kind: {}", raw_value))?;
            config
                .web
                .providers
                .entry(name.to_string())
                .or_insert_with(|| WebProviderConfigFile {
                    kind,
                    base_url: None,
                    credential_profile: None,
                    command: None,
                    output: None,
                    limits: Default::default(),
                })
                .kind = kind;
        }
        key if key.starts_with("web.providers.") && key.ends_with(".capabilities") => {
            return Err(read_only_web_provider_capabilities_key_error(key));
        }
        key if key.starts_with("web.providers.") && key.ends_with(".base_url") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".base_url").unwrap();
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name>.base_url requires a non-empty provider name"
                ));
            }
            let provider = config.web.providers.get_mut(name).ok_or_else(|| {
                anyhow!("web provider {name} not found; set web.providers.{name}.kind first")
            })?;
            let value = raw_value.trim();
            provider.base_url = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        key if key.starts_with("web.providers.") && key.ends_with(".credential_profile") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".credential_profile").unwrap();
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name>.credential_profile requires a non-empty provider name"
                ));
            }
            let provider = config.web.providers.get_mut(name).ok_or_else(|| {
                anyhow!("web provider {name} not found; set web.providers.{name}.kind first")
            })?;
            let value = raw_value.trim();
            provider.credential_profile = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        key if key.starts_with("web.providers.") && key.ends_with(".command.argv") => {
            let provider = web_provider_config_mut(config, key, ".command.argv")?;
            provider.command = Some(WebCommandProviderConfigFile {
                argv: parse_string_list(raw_value)?,
            });
        }
        key if key.starts_with("web.providers.") && key.ends_with(".output.format") => {
            let provider = web_provider_config_mut(config, key, ".output.format")?;
            let output = provider
                .output
                .get_or_insert_with(default_web_command_output);
            output.format =
                serde_json::from_value(serde_json::Value::String(raw_value.trim().to_string()))
                    .with_context(|| format!("invalid web command output format: {}", raw_value))?;
        }
        key if key.starts_with("web.providers.") && key.contains(".output.mapping.") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let (name, field) =
                web_provider_output_mapping_key(rest).ok_or_else(|| unknown_config_key(key))?;
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name>.output.mapping requires a non-empty provider name"
                ));
            }
            let provider = config.web.providers.get_mut(name).ok_or_else(|| {
                anyhow!("web provider {name} not found; set web.providers.{name}.kind first")
            })?;
            let output = provider
                .output
                .get_or_insert_with(default_web_command_output);
            set_output_mapping_field(&mut output.mapping, field, raw_value.trim());
        }
        key if key.starts_with("web.providers.") && key.ends_with(".limits.timeout_ms") => {
            let provider = web_provider_config_mut(config, key, ".limits.timeout_ms")?;
            provider.limits.timeout_ms = Some(parse_positive_u64_key(key, raw_value)?);
        }
        key if key.starts_with("web.providers.") && key.ends_with(".limits.max_output_bytes") => {
            let provider = web_provider_config_mut(config, key, ".limits.max_output_bytes")?;
            provider.limits.max_output_bytes = Some(parse_positive_usize_key(key, raw_value)?);
        }
        "agent_templates.remote_sources" => {
            let sources: std::collections::BTreeMap<
                String,
                crate::config::file::AgentTemplateRemoteSourceConfigFile,
            > = serde_json::from_str(raw_value).with_context(|| {
                format!("invalid agent_templates.remote_sources JSON: {}", raw_value)
            })?;
            config.agent_templates.remote_sources = sources;
        }
        key if key.starts_with("agent_templates.remote_sources.") => {
            let source_id = key.strip_prefix("agent_templates.remote_sources.").unwrap();
            if source_id.is_empty() {
                return Err(anyhow!(
                    "agent_templates.remote_sources.<id> requires a non-empty source id"
                ));
            }
            let source: crate::config::file::AgentTemplateRemoteSourceConfigFile =
                serde_json::from_str(raw_value).with_context(|| {
                    format!(
                        "invalid agent template source config for {}: {}",
                        source_id, raw_value
                    )
                })?;
            config
                .agent_templates
                .remote_sources
                .insert(source_id.to_string(), source);
        }
        _ => return Err(unknown_config_key(key)),
    }
    validate_api_cors_config(&config.api.cors)?;
    Ok(())
}

pub fn unset_config_key(config: &mut HolonConfigFile, key: &str) -> Result<()> {
    match key {
        "api.cors.enabled" => config.api.cors.enabled = None,
        "api.cors.allowed_origins" => config.api.cors.allowed_origins.clear(),
        "api.cors.allowed_methods" => {
            config.api.cors.allowed_methods = default_api_cors_allowed_methods()
        }
        "api.cors.allowed_headers" => {
            config.api.cors.allowed_headers = default_api_cors_allowed_headers()
        }
        "api.cors.allow_credentials" => config.api.cors.allow_credentials = None,
        "api.cors.max_age_seconds" => config.api.cors.max_age_seconds = Some(600),
        "model.default" => config.model.default = None,
        "model.fallbacks" => config.model.fallbacks.clear(),
        "vision.default" => config.vision.default = None,
        "image_generation.default" => config.image_generation.default = None,
        "models.catalog" => config.models.catalog.clear(),
        "model.unknown_fallback" => config.model.unknown_fallback = None,
        "model.unknown_fallback.context_window_tokens" => {
            clear_unknown_model_fallback_field(config, |value| value.context_window_tokens = None);
        }
        "model.unknown_fallback.effective_context_window_percent" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.effective_context_window_percent = None;
            });
        }
        "model.unknown_fallback.prompt_budget_estimated_tokens" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.prompt_budget_estimated_tokens = None;
            });
        }
        "model.unknown_fallback.compaction_trigger_estimated_tokens" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.compaction_trigger_estimated_tokens = None;
            });
        }
        "model.unknown_fallback.compaction_keep_recent_estimated_tokens" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.compaction_keep_recent_estimated_tokens = None;
            });
        }
        "model.unknown_fallback.runtime_max_output_tokens" => {
            clear_unknown_model_fallback_field(config, |value| {
                value.runtime_max_output_tokens = None;
            });
        }
        key if key.starts_with("providers.") => unset_provider_config_key(config, key)?,
        "runtime.max_output_tokens" => config.runtime.max_output_tokens = None,
        "runtime.default_tool_output_tokens" => config.runtime.default_tool_output_tokens = None,
        "runtime.max_tool_output_tokens" => config.runtime.max_tool_output_tokens = None,
        "runtime.disable_provider_fallback" => config.runtime.disable_provider_fallback = None,
        "tui.alternate_screen" => config.tui.alternate_screen = None,
        "web.fetch.enabled" => config.web.fetch.enabled = None,
        "web.fetch.max_chars" => config.web.fetch.max_chars = None,
        "web.fetch.max_response_bytes" => config.web.fetch.max_response_bytes = None,
        "web.fetch.timeout_seconds" => config.web.fetch.timeout_seconds = None,
        "web.fetch.max_redirects" => config.web.fetch.max_redirects = None,
        "web.fetch.allowed_hosts" => config.web.fetch.allowed_hosts.clear(),
        "web.fetch.denied_hosts" => config.web.fetch.denied_hosts.clear(),
        "web.search.enabled" => config.web.search.enabled = None,
        "web.search.builtin_provider.enabled" => config.web.search.builtin_provider.enabled = None,
        "web.search.provider" => config.web.search.provider = None,
        "web.search.mode" => config.web.search.mode = None,
        "web.search.providers" => config.web.search.providers.clear(),
        "web.search.max_results" => config.web.search.max_results = None,
        "web.search.max_provider_attempts" => config.web.search.max_provider_attempts = None,
        key if key.starts_with("web.providers.") && key.ends_with(".kind") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".kind").unwrap();
            if let Some(provider) = config.web.providers.get_mut(name) {
                provider.kind = WebProviderKind::DuckDuckGo;
            } else {
                return Err(anyhow!("web provider {name} not found"));
            }
        }
        key if key.starts_with("web.providers.") && key.ends_with(".capabilities") => {
            return Err(read_only_web_provider_capabilities_key_error(key));
        }
        key if key.starts_with("web.providers.") && key.ends_with(".base_url") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".base_url").unwrap();
            if let Some(provider) = config.web.providers.get_mut(name) {
                provider.base_url = None;
            } else {
                return Err(anyhow!("web provider {name} not found"));
            }
        }
        key if key.starts_with("web.providers.") && key.ends_with(".credential_profile") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let name = rest.strip_suffix(".credential_profile").unwrap();
            if let Some(provider) = config.web.providers.get_mut(name) {
                provider.credential_profile = None;
            } else {
                return Err(anyhow!("web provider {name} not found"));
            }
        }
        key if key.starts_with("web.providers.") && key.ends_with(".command.argv") => {
            let provider = web_provider_config_mut(config, key, ".command.argv")?;
            provider.command = None;
        }
        key if key.starts_with("web.providers.") && key.ends_with(".output.format") => {
            let provider = web_provider_config_mut(config, key, ".output.format")?;
            if let Some(output) = provider.output.as_mut() {
                output.format = WebCommandOutputFormatFile::Json;
            }
        }
        key if key.starts_with("web.providers.") && key.contains(".output.mapping.") => {
            let rest = key.strip_prefix("web.providers.").unwrap();
            let (name, field) =
                web_provider_output_mapping_key(rest).ok_or_else(|| unknown_config_key(key))?;
            let provider = config
                .web
                .providers
                .get_mut(name)
                .ok_or_else(|| anyhow!("web provider {name} not found"))?;
            if let Some(output) = provider.output.as_mut() {
                unset_output_mapping_field(&mut output.mapping, field);
            }
        }
        key if key.starts_with("web.providers.") && key.ends_with(".limits.timeout_ms") => {
            let provider = web_provider_config_mut(config, key, ".limits.timeout_ms")?;
            provider.limits.timeout_ms = None;
        }
        key if key.starts_with("web.providers.") && key.ends_with(".limits.max_output_bytes") => {
            let provider = web_provider_config_mut(config, key, ".limits.max_output_bytes")?;
            provider.limits.max_output_bytes = None;
        }
        key if key.starts_with("web.providers.") => {
            let name = key.strip_prefix("web.providers.").unwrap();
            if name.is_empty() {
                return Err(anyhow!(
                    "web.providers.<name> requires a non-empty provider name"
                ));
            }
            if config.web.providers.remove(name).is_none() {
                return Err(anyhow!("web provider {name} not found"));
            }
            return Ok(());
        }
        "agent_templates.remote_sources" => {
            config.agent_templates.remote_sources.clear();
        }
        key if key.starts_with("agent_templates.remote_sources.") => {
            let source_id = key.strip_prefix("agent_templates.remote_sources.").unwrap();
            if source_id.is_empty() {
                return Err(anyhow!(
                    "agent_templates.remote_sources.<id> requires a non-empty source id"
                ));
            }
            config.agent_templates.remote_sources.remove(source_id);
        }
        _ => return Err(unknown_config_key(key)),
    }
    validate_api_cors_config(&config.api.cors)?;
    Ok(())
}

pub fn validate_api_cors_config(cors: &ApiCorsConfigFile) -> Result<()> {
    if cors.allow_credentials() && cors.allowed_origins.iter().any(|origin| origin == "*") {
        return Err(anyhow!(
            "api.cors.allow_credentials cannot be true when api.cors.allowed_origins contains wildcard *"
        ));
    }
    for origin in &cors.allowed_origins {
        if origin != "*" {
            origin
                .parse::<HeaderValue>()
                .with_context(|| format!("invalid api.cors.allowed_origins entry {origin:?}"))?;
        }
    }
    for method in &cors.allowed_methods {
        method
            .parse::<Method>()
            .with_context(|| format!("invalid api.cors.allowed_methods entry {method:?}"))?;
    }
    for header in &cors.allowed_headers {
        header
            .parse::<HeaderName>()
            .with_context(|| format!("invalid api.cors.allowed_headers entry {header:?}"))?;
    }
    Ok(())
}

pub(crate) fn parse_string_list(raw_value: &str) -> Result<Vec<String>> {
    let trimmed = raw_value.trim();
    if trimmed.starts_with('[') {
        let values: Vec<String> =
            serde_json::from_str(trimmed).context("expected a JSON string array")?;
        return Ok(values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect());
    }
    Ok(trimmed
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect())
}

pub(crate) fn parse_bool_value(raw_value: &str) -> Result<Option<bool>> {
    match raw_value.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "true" | "1" | "yes" | "on" => Ok(Some(true)),
        "false" | "0" | "no" | "off" => Ok(Some(false)),
        _ => Err(anyhow!("expected boolean true|false|1|0|yes|no|on|off")),
    }
}

pub(crate) fn parse_comma_separated_values(raw_value: &str) -> Vec<String> {
    raw_value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

pub(crate) fn parse_positive_u32_key(key: &str, raw_value: &str) -> Result<u32> {
    raw_value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{key} expects a positive integer"))
}

pub(crate) fn parse_positive_u64_key(key: &str, raw_value: &str) -> Result<u64> {
    raw_value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{key} expects a positive integer"))
}

pub(crate) fn parse_positive_usize_key(key: &str, raw_value: &str) -> Result<usize> {
    raw_value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{key} expects a positive integer"))
}

pub(crate) fn parse_percentage_u8_key(key: &str, raw_value: &str) -> Result<u8> {
    raw_value
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|value| *value > 0 && *value <= 100)
        .ok_or_else(|| anyhow!("{key} expects an integer from 1 to 100"))
}

pub(crate) fn parse_url_value(key: &str, raw_value: &str) -> Result<()> {
    let trimmed = raw_value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{key} expects a non-empty URL"));
    }
    let parsed = reqwest::Url::parse(trimmed)
        .with_context(|| format!("{key} expects a valid absolute URL"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(anyhow!("{key} expects an http or https URL"));
    }
    Ok(())
}

pub(crate) fn is_startup_only_config_key(key: &str) -> bool {
    let _ = key;
    false
}

pub(crate) fn startup_only_config_key_error(key: &str) -> anyhow::Error {
    anyhow!(
        "config key {key} is startup-only; configure it via env vars or CLI startup flags instead of runtime config mutation"
    )
}

pub(crate) fn get_config_value(
    primary_env: &str,
    secondary_env: Option<&str>,
    settings_env: &HashMap<String, String>,
) -> Option<String> {
    env::var(primary_env)
        .ok()
        .or_else(|| secondary_env.and_then(|key| env::var(key).ok()))
        .or_else(|| settings_env.get(primary_env).cloned())
        .or_else(|| secondary_env.and_then(|key| settings_env.get(key).cloned()))
}

pub(crate) fn unknown_config_key(key: &str) -> anyhow::Error {
    if is_startup_only_config_key(key) {
        return startup_only_config_key_error(key);
    }
    if key.starts_with("web.providers.") {
        return anyhow!("unknown web providers config key {key}; mutable fields: .kind, .base_url, .credential_profile; .capabilities is derived read-only metadata; use web.providers.<name>.kind to create a provider first");
    }
    let supported = config_schema()
        .into_iter()
        .map(|entry| entry.key)
        .collect::<Vec<_>>();
    let suggestions = supported
        .iter()
        .filter(|candidate| candidate.contains(key) || key.contains(**candidate))
        .copied()
        .collect::<Vec<_>>();
    if suggestions.is_empty() {
        anyhow!(
            "unknown config key {key}; supported keys: {}",
            supported.join(", ")
        )
    } else {
        anyhow!(
            "unknown config key {key}; did you mean: {}",
            suggestions.join(", ")
        )
    }
}
