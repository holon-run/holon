//! Persisted Holon configuration and model route resolution.

use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use http::{HeaderName, HeaderValue, Method};
use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

use holon_domain::{
    CommandTaskOutputPolicy, ContextBudgetOverrides, ContextConfig, ViewImageSelectedMode,
    ViewImageVisionCandidate, ViewImageVisionSelection, DEFAULT_TOOL_OUTPUT_TOKENS,
    MAX_TOOL_OUTPUT_TOKENS,
};
use holon_model_catalog::{
    BuiltInModelCatalog, BuiltInModelMetadata, ModelRouteRuntimeOverride, ModelRuntimeOverride,
    ResolvedRuntimeModelPolicy,
};
use holon_provider::{
    auth::{codex_cli_auth_file_exists, load_codex_cli_credential},
    provider::ProviderNativeWebSearchKind,
};
use holon_storage::{
    retention_config::*, DEFAULT_ORDINARY_MAX_CONNECTIONS, DEFAULT_PREPARATION_MAX_CONNECTIONS,
};

pub mod authentication;
pub mod web_config;
use authentication::{AuthenticationMode, SessionPolicy};
use web_config::{
    WebFetchConfig, WebProviderKind, WebProviderLimitsConfig, WebSearchConfig, WebSearchMode,
};

mod builtin_providers;
mod credentials;
mod file;
mod models;
mod persistence;
mod provider_headers;
mod providers;
mod schema;
mod service_tier;
mod web;

pub use builtin_providers::*;
pub use credentials::*;
pub use file::*;
pub use models::*;
pub use persistence::*;
pub use provider_headers::*;
pub use providers::*;
pub use schema::*;
pub use service_tier::*;
pub use web::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AltScreenMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl AltScreenMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(anyhow!(
                "invalid alternate screen mode {other}; expected auto|always|never"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

pub fn load_settings_env() -> Result<HashMap<String, String>> {
    let path = settings_path();
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let settings: ClaudeSettings =
        serde_json::from_str(&content).context("failed to parse ~/.claude/settings.json")?;
    Ok(settings.env)
}

pub fn default_holon_home() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".holon")
}

pub fn default_user_home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
        })
        .or_else(|| {
            let drive = env::var_os("HOMEDRIVE")?;
            let path = env::var_os("HOMEPATH")?;
            let joined = PathBuf::from(drive).join(path);
            (!joined.as_os_str().is_empty()).then_some(joined)
        })
}

pub fn default_codex_home() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".codex")
}

fn settings_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".claude/settings.json")
}

#[doc(hidden)]
pub fn resolve_anthropic_context_management_config() -> Result<AnthropicContextManagementConfig> {
    resolve_anthropic_context_management_config_with_defaults(
        default_anthropic_runtime_cache_strategy(),
        true,
    )
}

#[doc(hidden)]
pub fn resolve_anthropic_compatible_context_management_config(
) -> Result<AnthropicContextManagementConfig> {
    resolve_anthropic_context_management_config_with_defaults(
        default_anthropic_runtime_cache_strategy(),
        false,
    )
}

fn resolve_anthropic_context_management_config_with_defaults(
    default_cache_strategy: AnthropicCacheStrategy,
    auto_prompt_cache_betas: bool,
) -> Result<AnthropicContextManagementConfig> {
    let enabled = match env::var("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT").ok() {
        Some(value) => parse_bool_value(&value)
            .map_err(|_| anyhow!("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT expects a boolean"))?
            .ok_or_else(|| anyhow!("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT expects a boolean"))?,
        None => false,
    };
    let trigger_input_tokens = env::var("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_TRIGGER_INPUT_TOKENS")
        .ok()
        .map(|value| {
            parse_positive_u32_key(
                "HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_TRIGGER_INPUT_TOKENS",
                &value,
            )
        })
        .transpose()?
        .unwrap_or(100_000);
    let keep_recent_tool_uses =
        env::var("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_KEEP_RECENT_TOOL_USES")
            .ok()
            .map(|value| {
                parse_positive_u32_key(
                    "HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_KEEP_RECENT_TOOL_USES",
                    &value,
                )
            })
            .transpose()?
            .unwrap_or(3);
    let clear_at_least_input_tokens =
        env::var("HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_CLEAR_AT_LEAST_INPUT_TOKENS")
            .ok()
            .map(|value| {
                parse_positive_u32_key(
                    "HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_CLEAR_AT_LEAST_INPUT_TOKENS",
                    &value,
                )
            })
            .transpose()?;
    let cache_strategy = env::var("HOLON_ANTHROPIC_CACHE_STRATEGY")
        .ok()
        .map(|value| parse_anthropic_cache_strategy_env(&value))
        .transpose()?
        .unwrap_or(default_cache_strategy);
    let betas_env = env::var("HOLON_ANTHROPIC_BETAS").ok();
    let betas = match betas_env {
        Some(value) => parse_comma_separated_values(&value),
        None if auto_prompt_cache_betas
            && cache_strategy == AnthropicCacheStrategy::ClaudeCodePromptCache =>
        {
            vec![
                "claude-code-20250219".to_string(),
                "prompt-caching-scope-2026-01-05".to_string(),
            ]
        }
        None => Vec::new(),
    };

    Ok(AnthropicContextManagementConfig {
        enabled,
        trigger_input_tokens,
        keep_recent_tool_uses,
        clear_at_least_input_tokens,
        cache_strategy,
        betas,
        cache_capabilities: AnthropicCacheCapabilities::default(),
    })
}

fn default_anthropic_runtime_cache_strategy() -> AnthropicCacheStrategy {
    AnthropicCacheStrategy::ClaudeCodePromptCache
}

#[doc(hidden)]
pub fn parse_anthropic_cache_strategy_env(raw_value: &str) -> Result<AnthropicCacheStrategy> {
    if raw_value.trim().is_empty() {
        return Ok(default_anthropic_runtime_cache_strategy());
    }
    parse_anthropic_cache_strategy(raw_value)
}

#[doc(hidden)]
pub fn parse_anthropic_cache_strategy(raw_value: &str) -> Result<AnthropicCacheStrategy> {
    match raw_value.trim().to_ascii_lowercase().as_str() {
        "messages_native" | "messages-native" | "native" | "current" => {
            Ok(AnthropicCacheStrategy::MessagesNative)
        }
        "claude_code_prompt_cache"
        | "claude-code-prompt-cache"
        | "claude_cli_like"
        | "claude-cli-like"
        | "claude" => {
            Ok(AnthropicCacheStrategy::ClaudeCodePromptCache)
        }
        _ => Err(anyhow!(
            "HOLON_ANTHROPIC_CACHE_STRATEGY expects messages_native, claude_code_prompt_cache, or a legacy alias"
        )),
    }
}

pub const DEFAULT_X_SEARCH_MODEL: &str = "grok-4.3";
pub const DEFAULT_X_SEARCH_TIMEOUT_SECONDS: u64 = 60;

#[doc(hidden)]
pub fn resolve_runtime_db_retention_policy(
    stored_config: &HolonConfigFile,
) -> Result<holon_storage::retention_config::RuntimeDbRetentionPolicy> {
    let configured = &stored_config.runtime.retention;
    let defaults = holon_storage::retention_config::RuntimeDbRetentionPolicy::default();
    holon_storage::retention_config::RuntimeDbRetentionPolicy {
        enabled: configured.enabled.unwrap_or(defaults.enabled),
        audit_events_days: configured
            .audit_events_days
            .unwrap_or(defaults.audit_events_days),
        transcript_entries_days: configured
            .transcript_entries_days
            .unwrap_or(defaults.transcript_entries_days),
        tool_executions_days: configured
            .tool_executions_days
            .unwrap_or(defaults.tool_executions_days),
        audit_events_min_rows_per_scope: configured
            .audit_events_min_rows_per_scope
            .unwrap_or(defaults.audit_events_min_rows_per_scope),
        transcript_entries_min_rows: configured
            .transcript_entries_min_rows
            .unwrap_or(defaults.transcript_entries_min_rows),
        tool_executions_min_rows: configured
            .tool_executions_min_rows
            .unwrap_or(defaults.tool_executions_min_rows),
        interval_hours: configured.interval_hours.unwrap_or(defaults.interval_hours),
        incremental_vacuum_pages: configured
            .incremental_vacuum_pages
            .unwrap_or(defaults.incremental_vacuum_pages),
    }
    .validate()
}

#[doc(hidden)]
pub fn resolve_disable_provider_fallback_override(
    env_override: Option<&str>,
    stored_config: &HolonConfigFile,
) -> Result<bool> {
    match env_override {
        Some(value) => parse_bool_value(value)
            .map_err(|_| anyhow!("HOLON_DISABLE_PROVIDER_FALLBACK expects a boolean"))?
            .ok_or_else(|| anyhow!("HOLON_DISABLE_PROVIDER_FALLBACK expects a boolean")),
        None => Ok(stored_config
            .runtime
            .disable_provider_fallback
            .unwrap_or(false)),
    }
}

#[cfg(test)]
mod tests;
