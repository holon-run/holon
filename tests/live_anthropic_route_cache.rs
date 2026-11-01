//! Opt-in, bounded real-provider check of exact-route Anthropic cache control.
//! Run with HOLON_LIVE_ROUTE_CACHE_ROUTES=provider@endpoint/model[,route...].
//! This verifies request policy and diagnostics, not guaranteed cache savings.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use holon::{
    config::{
        AppConfig, ModelRouteCapability, ModelRouteRef, ProviderTransportKind, RuntimeModelCatalog,
    },
    context::ContextConfig,
    prompt::PromptStability,
    provider::{
        build_provider_from_model_chain, ConversationMessage, ModelBlock, PromptContentBlock,
        ProviderPromptCache, ProviderPromptFrame, ProviderTurnRequest,
    },
};
use serde_json::json;

/// A manually driven coding review, not a synthetic cache benchmark. Run one
/// explicitly selected Token Plan route at a time with --ignored --nocapture,
/// then enter a coding follow-up on stdin after reading the first answer.
#[tokio::test]
#[ignore = "interactive coding session; requires an explicit route, public task file and credentials"]
async fn live_token_plan_coding_review_uses_builtin_cache_policy() -> Result<()> {
    let route_text = std::env::var("HOLON_LIVE_CODING_REVIEW_ROUTE")?;
    let (expected_enabled, expected_source) = match route_text.as_str() {
        "dashscope@token-plan/deepseek-v4.1-flash"
        | "dashscope@token-plan/deepseek-v4-flash-0731" => (false, "route_builtin"),
        "dashscope@token-plan/qwen3.8-flash" => (true, "endpoint_builtin"),
        _ => anyhow::bail!("select one of the three reviewed Token Plan routes"),
    };
    let task = std::fs::read_to_string(std::env::var("HOLON_LIVE_CODING_REVIEW_TASK")?)?;
    ensure!(!task.trim().is_empty() && task.len() <= 100_000);
    let route = ModelRouteRef::parse(&route_text)?;
    let mut config = AppConfig::load()?;
    let trace_home = tempfile::tempdir()?;
    config.home_dir = trace_home.path().to_path_buf();
    config.runtime_max_output_tokens = 2048;
    let resolved = RuntimeModelCatalog::from_config(&config)
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &route,
            ModelRouteCapability::Turn,
        )
        .with_context(|| format!("unavailable route {route_text}"))?;
    ensure!(resolved.provider_config().has_configured_credential());
    ensure!(resolved.provider_config().transport == ProviderTransportKind::AnthropicMessages);
    ensure!(
        resolved.provider_config().base_url.trim_end_matches('/')
            == "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        "the live coding review requires the actual Token Plan endpoint"
    );
    let policy = resolved
        .capabilities
        .endpoint
        .anthropic_cache
        .context("missing resolved cache policy")?;
    ensure!(
        policy.cache_control == expected_enabled
            && serde_json::to_value(policy.source)? == expected_source,
        "live coding review requires the built-in policy, without user overrides"
    );
    let provider = build_provider_from_model_chain(&config, std::slice::from_ref(&route))?;
    let frame = ProviderPromptFrame::structured(
        "你是只读代码审查助手。根据提供的公开源码审查指定改动，用中文简要指出具体缺陷或说明未发现阻断；不要声称运行过测试。",
        vec![],
        vec![PromptContentBlock {
            text: task,
            stability: PromptStability::TurnScoped,
            cache_breakpoint: true,
        }],
        Some(ProviderPromptCache {
            agent_id: "live-token-plan-coding-review".into(),
            prompt_cache_key: "live-token-plan-coding-review".into(),
            context_fingerprint: "public-coding-review".into(),
            compression_epoch: 0,
        }),
    );
    let mut conversation = vec![ConversationMessage::UserBlocks(
        frame.context_blocks.clone(),
    )];
    for round in 1..=2 {
        let response = tokio::time::timeout(
            Duration::from_secs(300),
            provider.complete_turn(ProviderTurnRequest {
                dispatch_guard: None,
                material: Default::default(),
                continuation_scope_id: None,
                prompt_frame: frame.clone(),
                conversation: conversation.clone(),
                tools: vec![],
                native_web_search: None,
                response_format: None,
            }),
        )
        .await
        .context("coding review exceeded 300 seconds")??;
        let diagnostics = response
            .request_diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.anthropic_cache.as_ref())
            .context("missing real request diagnostics")?;
        let evidence = serde_json::to_value(diagnostics)?;
        ensure!(evidence["model_route"] == route_text);
        ensure!(evidence["explicit_cache_control_enabled"] == expected_enabled);
        ensure!(evidence["cache_control_source"] == expected_source);
        let markers =
            diagnostics.system_cache_control_count + diagnostics.message_cache_control_count;
        ensure!((markers > 0) == expected_enabled);
        if !expected_enabled {
            ensure!(diagnostics.cache_breakpoints.is_empty());
            ensure!(!diagnostics.automatic_cache_control_requested);
        }
        let text = response
            .blocks
            .iter()
            .filter_map(|block| match block {
                ModelBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        ensure!(!text.trim().is_empty(), "missing coding review answer");
        println!(
            "LIVE_CODING_EVIDENCE {}",
            json!({
                "route": route_text, "round": round,
                "enabled": expected_enabled, "source": evidence["cache_control_source"],
                "strategy": diagnostics.cache_strategy, "markers": markers,
                "input_tokens": response.input_tokens, "output_tokens": response.output_tokens,
                "cache_usage": response.cache_usage
            })
        );
        println!("代码审查答复：\n{text}");
        conversation.push(ConversationMessage::AssistantBlocks(response.blocks));
        if round == 1 {
            println!("请输入一条代码审查追问以继续第二轮：");
            let mut followup = String::new();
            std::io::stdin().read_line(&mut followup)?;
            ensure!(
                !followup.trim().is_empty(),
                "interactive follow-up required"
            );
            conversation.push(ConversationMessage::UserText(followup));
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicit routes, configured credentials and network access; makes paid requests"]
async fn live_exact_route_cache_control_and_diagnostics() -> Result<()> {
    let routes = std::env::var("HOLON_LIVE_ROUTE_CACHE_ROUTES")
        .context("set HOLON_LIVE_ROUTE_CACHE_ROUTES to one to four exact Anthropic routes")?
        .split(',')
        .map(|route| {
            ensure!(
                route.contains('@'),
                "live cache probes require an explicit endpoint"
            );
            ModelRouteRef::parse_compatible(route.trim())
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!((1..=4).contains(&routes.len()), "choose one to four routes");

    let base = AppConfig::load()?;
    let trace_home = tempfile::tempdir()?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let prefix = (0..256)
        .map(|index| {
            format!(
                "Synthetic cache fact {index}: this is public test data; the answer is CACHE_ROUTE_OK.\n"
            )
        })
        .collect::<String>();

    for route in &routes {
        // Rebuild the real registry path A -> B -> A, never changing user config.
        for enabled in [false, true, false] {
            let mut config = base.clone();
            config.home_dir = trace_home.path().to_path_buf();
            config.runtime_max_output_tokens = 256;
            // Keep opposite policies resident on peer routes, including peers
            // sharing this endpoint, to catch endpoint-global state leakage.
            for peer in &routes {
                config.stored_config.models.routes.insert(
                    peer.as_string(),
                    serde_json::from_value(json!({
                        "cache_capabilities": { "cache_control": !enabled }
                    }))?,
                );
            }
            config.stored_config.models.routes.insert(
                route.as_string(),
                serde_json::from_value(json!({
                    "cache_capabilities": { "cache_control": enabled }
                }))?,
            );
            let resolved = RuntimeModelCatalog::from_config(&config)
                .resolve_explicit_model_route(
                    &ContextConfig::default(),
                    route,
                    ModelRouteCapability::Turn,
                )
                .with_context(|| format!("unavailable route {}", route.as_string()))?;
            ensure!(
                resolved.provider_config().transport == ProviderTransportKind::AnthropicMessages,
                "select an Anthropic Messages route"
            );
            ensure!(
                resolved.provider_config().has_configured_credential(),
                "the selected route needs an existing credential"
            );
            let provider = build_provider_from_model_chain(&config, std::slice::from_ref(route))?;
            let frame = ProviderPromptFrame::structured(
                format!("Live route-cache probe {nonce}. Reply only CACHE_ROUTE_OK.\n{prefix}"),
                vec![],
                vec![PromptContentBlock {
                    text: "Current probe answer: CACHE_ROUTE_OK.".into(),
                    stability: PromptStability::TurnScoped,
                    cache_breakpoint: true,
                }],
                Some(ProviderPromptCache {
                    agent_id: "live-route-cache".into(),
                    prompt_cache_key: format!("live-route-cache-{nonce}"),
                    context_fingerprint: format!("synthetic-prefix-{nonce}"),
                    compression_epoch: 0,
                }),
            );
            let mut conversation = vec![ConversationMessage::UserText(
                "What is the current probe answer?".into(),
            )];
            for round in 0..2 {
                let response = tokio::time::timeout(
                    Duration::from_secs(90),
                    provider.complete_turn(ProviderTurnRequest {
                        dispatch_guard: None,
                        material: Default::default(),
                        continuation_scope_id: None,
                        prompt_frame: frame.clone(),
                        conversation: conversation.clone(),
                        tools: vec![],
                        native_web_search: None,
                        response_format: None,
                    }),
                )
                .await
                .context("real cache probe exceeded 90 seconds")??;
                let text = response
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ModelBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                ensure!(text.contains("CACHE_ROUTE_OK"), "missing live probe answer");
                let diagnostics = response
                    .request_diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.anthropic_cache.as_ref())
                    .context("missing Anthropic request diagnostics")?;
                let evidence = serde_json::to_value(diagnostics)?;
                ensure!(evidence["explicit_cache_control_enabled"] == enabled);
                ensure!(evidence["cache_control_source"] == "route_override");
                ensure!(evidence["model_route"] == resolved.route_ref.as_string());
                let markers = diagnostics.system_cache_control_count
                    + diagnostics.message_cache_control_count;
                if enabled {
                    ensure!(
                        markers > 0,
                        "enabled synthetic prompt must request a breakpoint"
                    );
                } else {
                    ensure!(markers == 0, "disabled route emitted cache breakpoints");
                    ensure!(!diagnostics.automatic_cache_control_requested);
                }
                println!(
                    "route={} enabled={enabled} round={round} source={} markers={markers} input={} output={} cache={:?}",
                    resolved.route_ref.as_string(),
                    evidence["cache_control_source"],
                    response.input_tokens,
                    response.output_tokens,
                    response.cache_usage
                );
                conversation.push(ConversationMessage::AssistantBlocks(response.blocks));
                conversation.push(ConversationMessage::UserText(
                    "Repeat the current probe answer.".into(),
                ));
            }
        }
    }
    Ok(())
}
