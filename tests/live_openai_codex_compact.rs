use anyhow::{anyhow, Result};
use holon::{
    config::{AppConfig, ProviderId},
    provider::{
        AgentProvider, ConversationMessage, OpenAiCodexProvider, ProviderPromptCache,
        ProviderPromptFrame, ProviderTurnRequest,
    },
};

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

fn live_openai_codex_model() -> String {
    std::env::var("HOLON_LIVE_OPENAI_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.3-codex-spark".into())
}

fn codex_compact_route(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    let api_base = if base_url.ends_with("/codex") {
        base_url.to_string()
    } else {
        format!("{base_url}/codex")
    };
    format!("{api_base}/responses/compact")
}

#[tokio::test]
#[ignore = "requires Codex CLI ChatGPT auth and network access"]
async fn live_openai_codex_remote_compact_route_probe() -> Result<()> {
    let config = live_config()?;
    let provider_config = config
        .providers
        .get(&ProviderId::openai_codex())
        .ok_or_else(|| anyhow!("missing openai-codex provider config"))?;
    let route = codex_compact_route(&provider_config.base_url);
    let provider = OpenAiCodexProvider::from_config(&config, &live_openai_codex_model())?;
    let output = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: ProviderPromptFrame::structured(
                "Reply briefly. Do not include private information.",
                Vec::new(),
                Vec::new(),
                Some(ProviderPromptCache {
                    agent_id: "live-openai-codex-compact-probe".into(),
                    prompt_cache_key: "live-openai-codex-compact-probe".into(),
                    working_memory_revision: 0,
                    compression_epoch: 0,
                }),
            ),
            conversation: (0..8)
                .map(|index| ConversationMessage::UserText(format!("compact probe item {index}")))
                .collect(),
            tools: vec![],
        })
        .await?;

    let diagnostics = output
        .request_diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.openai_remote_compaction.as_ref())
        .ok_or_else(|| anyhow!("remote compact was not attempted for route {route}"))?;

    eprintln!(
        "openai-codex compact route probe: route={route}, status={}, http_status_class={}, input_items={:?}, output_items={:?}, compaction_items={:?}",
        diagnostics.status,
        diagnostics
            .http_status
            .map(|status| format!("{}xx", status / 100))
            .unwrap_or_else(|| "none".into()),
        diagnostics.input_items,
        diagnostics.output_items,
        diagnostics.compaction_items
    );

    if diagnostics.http_status == Some(404) {
        anyhow::bail!("openai-codex compact route returned 404: {route}");
    }
    Ok(())
}
