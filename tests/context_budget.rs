//! Runtime budget regression using an isolated home and a loopback-only provider.

use std::{
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::{ensure, Context, Result};
use axum::{routing::post, Json, Router};
use holon::{
    config::{AppConfig, ModelRef, ModelRouteRef, ProviderId},
    host::{AgentBootstrapExecution, RuntimeHost},
    model_catalog::{
        BuiltInModelCatalog, ModelMetadataField, ModelMetadataOrigin, ModelMetadataSource,
    },
    model_discovery::ProviderModelDiscoveryCache,
    runtime::RuntimeHandle,
    types::{AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority},
};
use serde_json::{json, Value};
use tokio::time::{Duration, Instant};

const CHILD_MARKER: &str = "HOLON_CONTEXT_BUDGET_TEST_CHILD";
const TEST_NAME: &str =
    "incomplete_context_metadata_survives_initialization_tools_and_model_switches";
const AGENT_ID: &str = "budget-new-agent";
const PARTIAL_MODEL: &str = "opencode-go@default/iceman-contributor-free";

#[test]
fn incomplete_context_metadata_survives_initialization_tools_and_model_switches() -> Result<()> {
    if std::env::var_os(CHILD_MARKER).is_none() {
        // Keep environment-based configuration and all runtime databases out of
        // both the developer's home and the other integration-test processes.
        let root = tempfile::tempdir()?;
        let home = root.path().join("holon");
        let user_home = root.path().join("user");
        let workspace = root.path().join("workspace");
        for dir in [&home, &user_home, &workspace] {
            std::fs::create_dir_all(dir)?;
        }
        let output = Command::new(std::env::current_exe()?)
            .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
            .env_clear()
            .env(CHILD_MARKER, "1")
            .env("HOLON_HOME", &home)
            .env("HOME", &user_home)
            .env("HOLON_WORKSPACE_DIR", &workspace)
            .env("RUST_MIN_STACK", "16777216")
            .current_dir(&workspace)
            .output()?;
        ensure!(
            output.status.success(),
            "isolated budget regression failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return Ok(());
    }

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()?
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(60), exercise_budget_lifecycle()).await?
        })
}

async fn exercise_budget_lifecycle() -> Result<()> {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                let index = {
                    let mut captured = captured.lock().unwrap();
                    let index = captured.len();
                    captured.push(body);
                    index
                };
                let (message, finish_reason) = if index == 0 {
                    (
                        json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "budget-get-agent",
                                "type": "function",
                                "function": {
                                    "name": "GetAgent",
                                    "arguments": "{\"detail\":\"full\"}"
                                }
                            }]
                        }),
                        "tool_calls",
                    )
                } else {
                    let text = match index {
                        1 => "budget initialization completed",
                        2 => "budget later turn completed",
                        3 => "budget known model completed",
                        4 => "budget unknown model completed",
                        5 => "budget discovered model completed",
                        _ => "unexpected budget request",
                    };
                    (json!({"role": "assistant", "content": text}), "stop")
                };
                Json(json!({
                    "id": format!("budget-response-{index}"),
                    "object": "chat.completion",
                    "created": 1,
                    "model": "loopback-budget-stub",
                    "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                }))
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let home = std::path::PathBuf::from(std::env::var_os("HOLON_HOME").context("isolated home")?);
    std::fs::write(
        home.join("config.json"),
        serde_json::to_vec_pretty(&json!({
            "model": {"default": PARTIAL_MODEL},
            "models": {"catalog": {
                "opencode-go/budget-known": {"context_window_tokens": 256_000}
            }},
            "runtime": {"disable_provider_fallback": true, "max_output_tokens": 777},
            "providers": {"opencode-go": {
                "transport": "openai_chat_completions",
                "base_url": format!("http://{address}/v1"),
                "auth": {"source": "none", "kind": "none"}
            }}
        }))?,
    )?;
    let mut config = AppConfig::load()?;
    assert_eq!(config.home_dir, home);
    assert_eq!(config.prompt_budget_estimated_tokens, 128_000);
    assert_eq!(config.compaction_trigger_estimated_tokens, 115_200);
    assert_eq!(config.compaction_keep_recent_estimated_tokens, 43_776);

    let catalog = BuiltInModelCatalog::new();
    let mut discovered = catalog
        .get(&ModelRef::parse("opencode-go/iceman-contributor-free")?)
        .context("partial builtin fixture")?
        .clone();
    assert_eq!(discovered.context_window_tokens, None);
    discovered.model_ref = ModelRef::parse("opencode-go/budget-discovered")?;
    discovered.display_name = "Partial discovery budget fixture".into();
    discovered.source = ModelMetadataSource::RemoteDiscovered;
    let provider_id = ProviderId::parse("opencode-go")?;
    config.model_discovery_cache.providers.insert(
        provider_id.clone(),
        ProviderModelDiscoveryCache {
            provider: provider_id,
            fetched_at: chrono::Utc::now(),
            source_url: Some(format!("http://{address}/v1/models")),
            response_hash: None,
            models: vec![discovered],
        },
    );

    // Use the real transport and tool dispatcher. No inference request can
    // leave loopback: the sole configured route has no fallback or credential.
    let host = RuntimeHost::new(config)?;
    host.create_public_named_agent_with_name(
        AGENT_ID,
        None,
        None,
        None,
        Some("Budget fixture"),
        AgentBootstrapExecution::Inline,
        true,
    )
    .await?;
    let runtime = host.get_public_agent(AGENT_ID).await?;
    wait_for_brief(&runtime, "budget initialization completed").await?;
    assert_unknown_budget(&runtime).await?;

    enqueue(&runtime, "run a subsequent turn").await?;
    wait_for_brief(&runtime, "budget later turn completed").await?;
    assert_unknown_budget(&runtime).await?;

    let known = runtime
        .set_model_override(
            ModelRouteRef::parse("opencode-go@default/budget-known")?,
            None,
        )
        .await?;
    assert_eq!(known.resolved_policy.context_window_tokens, Some(256_000));
    assert_eq!(
        known.resolved_policy.prompt_budget_estimated_tokens,
        243_200
    );
    enqueue(&runtime, "use the explicit known-context model").await?;
    wait_for_brief(&runtime, "budget known model completed").await?;

    runtime
        .set_model_override(
            ModelRouteRef::parse("opencode-go@default/budget-unknown")?,
            None,
        )
        .await?;
    assert_unknown_budget(&runtime).await?;
    enqueue(&runtime, "return to the unknown-context default").await?;
    wait_for_brief(&runtime, "budget unknown model completed").await?;

    runtime
        .set_model_override(
            ModelRouteRef::parse("opencode-go@default/budget-discovered")?,
            None,
        )
        .await?;
    assert_unknown_budget(&runtime).await?;
    enqueue(&runtime, "use partial discovered metadata").await?;
    wait_for_brief(&runtime, "budget discovered model completed").await?;

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 6);
    let expected_models = [
        "iceman-contributor-free",
        "iceman-contributor-free",
        "iceman-contributor-free",
        "budget-known",
        "budget-unknown",
        "budget-discovered",
    ];
    for (request, expected_model) in requests.iter().zip(expected_models) {
        assert_eq!(request["model"], expected_model);
        assert_eq!(request["max_tokens"], 777);
    }
    let tool_result = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .context("real GetAgent tool result in continuation")?;
    assert_eq!(tool_result["tool_call_id"], "budget-get-agent");
    let tool_result: Value = serde_json::from_str(tool_result["content"].as_str().unwrap())?;
    assert_eq!(
        tool_result.pointer("/result/agent/model/resolved_policy/prompt_budget_estimated_tokens"),
        Some(&json!(128_000)),
    );

    host.shutdown().await?;
    server.abort();
    Ok(())
}

async fn assert_unknown_budget(runtime: &RuntimeHandle) -> Result<()> {
    let summary = runtime.agent_summary().await?;
    let policy = &summary.model.resolved_policy;
    assert_eq!(policy.context_window_tokens, None);
    assert_eq!(policy.prompt_budget_estimated_tokens, 128_000);
    assert_eq!(policy.compaction_trigger_estimated_tokens, 115_200);
    assert_eq!(policy.compaction_keep_recent_estimated_tokens, 43_776);
    assert_eq!(
        policy
            .evidence
            .source_for(ModelMetadataField::PromptBudgetEstimatedTokens),
        Some(ModelMetadataOrigin::RuntimeDefault),
    );
    Ok(())
}

async fn enqueue(runtime: &RuntimeHandle, text: &str) -> Result<()> {
    runtime
        .enqueue(MessageEnvelope::new(
            AGENT_ID,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.into() },
        ))
        .await?;
    Ok(())
}

async fn wait_for_brief(runtime: &RuntimeHandle, text: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if runtime
            .recent_briefs(20)
            .await?
            .iter()
            .any(|brief| brief.text.contains(text))
            && runtime.agent_state().await?.current_run_id.is_none()
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "missing final budget result: {text}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
