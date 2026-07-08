//! Holon documentation generator.
//!
//! Generates markdown reference docs from built-in provider and model runtime metadata.
//! Run with: `cargo run --bin holon-docgen -- models > docs/website/reference/models.md`.
//!
//! Run in a clean environment (no HOLON_* or provider-specific env overrides) for
//! deterministic output that reflects the true built-in defaults.

use std::collections::BTreeSet;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: holon-docgen <command>");
        eprintln!("  models  - Generate models reference markdown");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "models" => generate_models_doc()?,
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            std::process::exit(1);
        }
    }
    Ok(())
}

use holon::config::ProviderTransportKind;

fn transport_display(transport: &ProviderTransportKind) -> String {
    match transport {
        ProviderTransportKind::AnthropicMessages => "Anthropic Messages".to_string(),
        ProviderTransportKind::GeminiGenerateContent => "Gemini Generate Content".to_string(),
        ProviderTransportKind::OpenAiResponses => "OpenAI Responses".to_string(),
        ProviderTransportKind::OpenAiChatCompletions => "OpenAI Chat Completions".to_string(),
        ProviderTransportKind::OpenAiCodexResponses => "OpenAI Codex".to_string(),
    }
}

fn format_tokens(tokens: impl std::fmt::Display) -> String {
    tokens.to_string()
}

fn generate_models_doc() -> anyhow::Result<()> {
    let catalog = holon::model_catalog::BuiltInModelCatalog::new();
    let models = catalog.list();
    let providers = holon::config::built_in_provider_doc_entries()?;
    let provider_accounts = providers
        .iter()
        .map(|entry| entry.provider.as_str())
        .collect::<BTreeSet<_>>();

    // Print header — use print! to avoid an extra blank line after the header
    // separator row, which would break Markdown table rendering.
    print!(
        r#"---
title: Supported Models
description: Complete reference of all built-in models and providers supported by Holon.
generated: auto-generated from holon source — do not edit directly
---

# Supported Models

Holon includes built-in configuration for **{provider_account_count} provider accounts**
across **{endpoint_count} endpoints** and **{model_count} models**.

This page is auto-generated from the Holon source code (`src/model_catalog.rs` and `src/config.rs`).
Run `cargo run --bin holon-docgen -- models > docs/website/reference/models.md` to regenerate.

Note: subscription-scoped providers such as `dashscope-token-plan` and
`dashscope-coding-plan` are intended for interactive AI coding/agent tool usage
under the upstream service terms, not backend automation or generic scripts.

## Provider Setup

Each provider account endpoint requires an API key or credential to use. Set the listed
environment variable before running Holon. `Legacy Provider Ref` is the user-visible provider id
used in existing `provider/model` refs and config shortcuts.

| Provider Account | Endpoint | Legacy Provider Ref | Transport | Base URL | Auth Env Variable(s) |
|------------------|----------|---------------------|-----------|----------|----------------------|
"#,
        provider_account_count = provider_accounts.len(),
        endpoint_count = providers.len(),
        model_count = models.len(),
    );

    for entry in &providers {
        let transport_str = transport_display(&entry.transport);
        let env_display = entry.auth_env.as_deref().unwrap_or("—");

        println!(
            "| `{provider}` | `{endpoint}` | `{legacy_provider}` | {transport} | `{url}` | `{env}` |",
            provider = entry.provider.as_str(),
            endpoint = entry.endpoint.as_str(),
            legacy_provider = entry.legacy_provider.as_str(),
            transport = transport_str,
            url = entry.base_url,
            env = env_display,
        );
    }

    // Print models section
    print!(
        r#"
## Model Catalog

The table below lists every built-in model with its context window, max output tokens,
and capabilities.

| Provider | Model | Usage | Context Window | Max Output | Reasoning | Image |
|----------|-------|-------|----------------|------------|-----------|-------|
"#
    );

    let mut sorted_models: Vec<_> = models.iter().collect();
    sorted_models.sort_by(|a, b| {
        a.model_ref
            .provider
            .as_str()
            .cmp(b.model_ref.provider.as_str())
            .then_with(|| a.model_ref.model.cmp(&b.model_ref.model))
    });

    for m in &sorted_models {
        println!(
            "| `{provider}` | `{model}` | `{provider}/{model}` | {ctx} | {max_out} | {reasoning} | {image} |",
            provider = m.model_ref.provider.as_str(),
            model = m.model_ref.model,
            ctx = m.context_window_tokens.map_or("—".to_string(), format_tokens),
            max_out = m.default_max_output_tokens.map_or("—".to_string(), format_tokens),
            reasoning = if m.capabilities.supports_reasoning { "✅" } else { "—" },
            image = if m.capabilities.image_input { "✅" } else { "—" },
        );
    }

    let provider_account_count = provider_accounts.len();
    let endpoint_count = providers.len();
    let model_count = models.len();
    eprintln!(
        "Generated model reference: {provider_account_count} provider accounts, {endpoint_count} endpoints, {model_count} models."
    );
    Ok(())
}
