//! Live provider smoke test — verifies transport-layer correctness for every
//! configured provider.
//!
//! This test is `#[ignore]` by default so it never runs in CI. Run manually:
//!
//! ```sh
//! cargo test --test live_provider_smoke -- --ignored --nocapture
//! ```
//!
//! The test loads the real holon config (including API credentials), enumerates
//! every provider that has a credential, and sends a minimal prompt through the
//! provider's transport code path. This catches issues that unit tests cannot:
//! token expiry, parameter serialization problems, transport-specific quirks,
//! and credential misconfiguration.
//!
//! Providers and models are **not** hardcoded — they are discovered dynamically
//! from the config so the test stays in sync with whatever providers are
//! configured at run time.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use holon::{
    config::{AppConfig, ModelRef, ModelRouteRef, ProviderId, ProviderTransportKind},
    context::ContextConfig,
    model_catalog::{BuiltInModelCatalog, ResolvedRuntimeModelPolicy},
    provider::{
        AgentProvider, AnthropicProvider, ConversationMessage, GeminiProvider, ModelBlock,
        OpenAiChatCompletionsProvider, OpenAiCodexProvider, OpenAiProvider, ProviderTurnRequest,
    },
};
use tempfile::TempDir;

/// Smoke test result for a single provider.
struct SmokeResult {
    provider: String,
    model: String,
    transport: String,
    status: &'static str,
    detail: String,
}

/// Build the provider→model mapping from the config's provider chain.
/// Only providers that appear in the chain (i.e. have a valid credential) are
/// included.
fn provider_model_map(config: &AppConfig) -> BTreeMap<ProviderId, String> {
    let chain: Vec<ModelRouteRef> = config.provider_chain();
    let mut map = BTreeMap::new();
    for model_ref in chain {
        map.entry(model_ref.provider.clone())
            .or_insert_with(|| model_ref.model.clone());
    }
    map
}

fn extract_reply_text(blocks: &[ModelBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

/// Resolve the model's runtime policy from config + catalog, mirroring what the
/// runtime does in `provider/catalog.rs`.
fn resolve_model_policy(config: &AppConfig, model_ref: &ModelRef) -> ResolvedRuntimeModelPolicy {
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
        model_ref,
        &config.validated_model_overrides,
        &config.model_discovery_cache.models(),
        config.validated_unknown_model_fallback.as_ref(),
        &base_context_config,
        config.runtime_max_output_tokens,
    )
}

#[tokio::test]
#[ignore = "requires real API credentials and network access"]
async fn live_all_providers_smoke() -> Result<()> {
    let config = AppConfig::load().context("failed to load holon config")?;
    let model_map = provider_model_map(&config);

    if model_map.is_empty() {
        eprintln!("No providers with credentials found in config — nothing to test.");
        return Ok(());
    }

    eprintln!(
        "Testing {} provider(s) with credentials from config…\n",
        model_map.len()
    );

    let trace_dir = TempDir::new()?;
    let mut results = Vec::new();

    for (provider_id, model) in &model_map {
        let provider_cfg = match config.providers.get(provider_id) {
            Some(cfg) => cfg,
            None => {
                results.push(SmokeResult {
                    provider: provider_id.as_str().into(),
                    model: model.clone(),
                    transport: "unknown".into(),
                    status: "SKIP",
                    detail: "provider not in registry".into(),
                });
                continue;
            }
        };

        if !provider_cfg.has_configured_credential() {
            results.push(SmokeResult {
                provider: provider_id.as_str().into(),
                model: model.clone(),
                transport: provider_cfg.transport.as_str().into(),
                status: "SKIP",
                detail: "no credential configured".into(),
            });
            continue;
        }

        let model_ref = ModelRef::new(provider_id.clone(), model.clone());
        let supports_reasoning = resolve_model_policy(&config, &model_ref)
            .capabilities
            .supports_reasoning;

        let result = smoke_one_provider(
            provider_cfg,
            provider_id,
            model,
            supports_reasoning,
            config.runtime_max_output_tokens,
            trace_dir.path(),
        )
        .await;

        results.push(result);
    }

    // Print summary table.
    eprintln!(
        "\n{:<25} {:<30} {:<28} {:<6} {}",
        "Provider", "Model", "Transport", "Status", "Detail"
    );
    eprintln!("{}", "-".repeat(120));
    let mut pass = 0usize;
    let mut fail = 0usize;
    for r in &results {
        if r.status == "PASS" {
            pass += 1;
        } else if r.status == "FAIL" {
            fail += 1;
        }
        eprintln!(
            "{:<25} {:<30} {:<28} {:<6} {}",
            r.provider, r.model, r.transport, r.status, r.detail
        );
    }
    eprintln!(
        "\nSummary: {} PASS, {} FAIL, {} total",
        pass,
        fail,
        results.len()
    );

    // Fail the test if any provider failed.
    let any_fail = results.iter().any(|r| r.status == "FAIL");
    assert!(!any_fail, "one or more provider smoke tests failed");

    Ok(())
}

/// Instantiate the provider based on its transport kind and send a minimal
/// prompt. Returns a `SmokeResult` with PASS/FAIL.
async fn smoke_one_provider(
    provider_cfg: &holon::config::ProviderRuntimeConfig,
    provider_id: &ProviderId,
    model: &str,
    supports_reasoning: bool,
    max_output_tokens: u32,
    trace_dir: &std::path::Path,
) -> SmokeResult {
    let transport = provider_cfg.transport;
    let provider_name = provider_id.as_str().to_string();

    let instantiate = || -> Result<Box<dyn AgentProvider>> {
        match transport {
            ProviderTransportKind::AnthropicMessages => {
                Ok(Box::new(AnthropicProvider::from_runtime_config(
                    provider_cfg,
                    model,
                    max_output_tokens,
                    trace_dir,
                    supports_reasoning,
                )?))
            }
            ProviderTransportKind::OpenAiChatCompletions => Ok(Box::new(
                OpenAiChatCompletionsProvider::from_runtime_config(
                    provider_cfg,
                    model,
                    max_output_tokens,
                    trace_dir,
                )?,
            )),
            ProviderTransportKind::OpenAiResponses => {
                Ok(Box::new(OpenAiProvider::from_runtime_config(
                    provider_cfg,
                    model,
                    max_output_tokens,
                    trace_dir,
                )?))
            }
            ProviderTransportKind::OpenAiCodexResponses => {
                Ok(Box::new(OpenAiCodexProvider::from_runtime_config(
                    provider_cfg,
                    model,
                    max_output_tokens,
                    trace_dir,
                    supports_reasoning,
                )?))
            }
            ProviderTransportKind::GeminiGenerateContent => {
                Ok(Box::new(GeminiProvider::from_runtime_config(
                    provider_cfg,
                    model,
                    max_output_tokens,
                    trace_dir,
                )?))
            }
        }
    };

    let provider = match instantiate() {
        Ok(p) => p,
        Err(e) => {
            return SmokeResult {
                provider: provider_name,
                model: model.into(),
                transport: transport.as_str().into(),
                status: "FAIL",
                detail: format!("instantiation error: {e:#}"),
            };
        }
    };

    let request = ProviderTurnRequest::plain(
        "You are a connectivity test. Reply with exactly: OK",
        vec![ConversationMessage::UserText(
            "Reply with exactly: OK".into(),
        )],
        vec![],
    );

    match provider.complete_turn(request).await {
        Ok(response) => {
            let reply = extract_reply_text(&response.blocks);
            SmokeResult {
                provider: provider_name,
                model: model.into(),
                transport: transport.as_str().into(),
                status: "PASS",
                detail: format!("reply=\"{reply}\""),
            }
        }
        Err(e) => SmokeResult {
            provider: provider_name,
            model: model.into(),
            transport: transport.as_str().into(),
            status: "FAIL",
            detail: format!("{e:#}"),
        },
    }
}
