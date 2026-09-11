use std::sync::Arc;

use anyhow::Result;
use holon::{
    config::{AppConfig, ControlAuthMode},
    host::RuntimeHost,
    provider::{
        test_support::{ScriptedAgentProvider, ScriptedProviderStep},
        ConversationMessage,
    },
    types::{AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority},
};
use serde_json::json;
use tempfile::tempdir;
use tokio::time::{Duration, Instant};

fn test_config() -> AppConfig {
    let home_dir = tempdir().unwrap().keep();
    AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
        user_home_dir: None,
        home_dir: home_dir.clone(),
        data_dir: home_dir.clone(),
        socket_path: home_dir.join("run").join("holon.sock"),
        workspace_dir: tempdir().unwrap().keep(),
        context_window_messages: 8,
        context_window_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        prompt_budget_estimated_tokens: 4096,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        control_token: Some("secret".into()),
        control_auth_mode: ControlAuthMode::Auto,
        auth: Default::default(),
        api_cors: Default::default(),
        api_projection: Default::default(),
        config_file_path: home_dir.join("config.json"),
        stored_config: Default::default(),
        default_model: holon::config::ModelRouteRef::parse_compatible(
            "anthropic/claude-sonnet-4-6",
        )
        .unwrap(),
        fallback_models: Vec::new(),
        vision_model: None,
        image_generation_model: None,
        vision_candidate_models: Vec::new(),
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: 8_000,
        max_tool_output_tokens: 64_000,
        disable_provider_fallback: false,
        tui_alternate_screen: holon::config::AltScreenMode::Auto,
        validated_model_overrides: std::collections::HashMap::new(),
        validated_unknown_model_fallback: None,
        model_discovery_cache: Default::default(),
        providers: holon::config::provider_registry_for_tests(
            None,
            Some("dummy"),
            home_dir.join(".codex"),
        ),
        web_config: holon::web::WebConfig::default(),
    }
}

async fn wait_until(predicate: impl Fn() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if predicate()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for condition"))
}

async fn wait_until_async<F, Fut>(predicate: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if predicate().await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for condition"))
}

#[tokio::test]
async fn scripted_provider_recovers_full_memory_through_bounded_command_results() -> Result<()> {
    let mut config = test_config();
    config.prompt_budget_estimated_tokens = 100_000;
    config.compaction_trigger_estimated_tokens = 80_000;
    config.compaction_keep_recent_estimated_tokens = 40_000;
    let source = format!("{}\r\nEND 🦀", "中文🦀\"\\\n".repeat(9000));
    let chars = source.chars().collect::<Vec<_>>();
    let mut steps = vec![ScriptedProviderStep::tool_use(
        "memory-source",
        "MemoryGet",
        json!({"source_ref": "agent_memory:self", "max_chars": 50_000}),
    )];
    let mut chunks = Vec::new();
    for (index, chunk) in chars.chunks(4000).enumerate() {
        let start = index * 4000;
        let end = start + chunk.len();
        let code = format!(
            "import glob,sys; p=glob.glob({}+'/**/tool-artifacts/memory-source-*.log',recursive=True)[0]; sys.stdout.write(open(p,encoding='utf-8',newline='').read()[{start}:{end}])",
            serde_json::to_string(&config.home_dir.display().to_string())?
        );
        let cmd = format!("python3 -c '{}'", code.replace('\'', "'\\''"));
        let id = format!("range-{index}");
        let (name, input) = if index % 2 == 0 {
            (
                "ExecCommand",
                json!({"cmd": cmd, "max_output_tokens": 5000}),
            )
        } else {
            (
                "ExecCommandBatch",
                json!({"items": [{"cmd": cmd}], "max_output_tokens": 5000}),
            )
        };
        steps.push(ScriptedProviderStep::tool_use(&id, name, input));
        chunks.push((id, chunk.iter().collect::<String>()));
    }
    steps.push(ScriptedProviderStep::text("full source recovered"));
    let provider = ScriptedAgentProvider::new(steps);
    let captured = provider.clone();
    let host = RuntimeHost::new_with_provider(config, Arc::new(provider))?;
    let runtime = host.default_runtime().await?;
    std::fs::create_dir_all(runtime.storage().data_dir().join("memory"))?;
    std::fs::write(runtime.storage().data_dir().join("memory/self.md"), &source)?;
    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Read the source snapshot in bounded ranges.".into(),
            },
        ))
        .await?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while captured.request_count() < chunks.len() + 2 {
        anyhow::ensure!(
            Instant::now() < deadline,
            "provider did not finish range reads"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let requests = captured.requests();
    let delivered = |id: &str| {
        requests.iter().find_map(|request| {
            request
                .conversation
                .iter()
                .find_map(|message| match message {
                    ConversationMessage::UserToolResults(results) => {
                        results.iter().find(|result| result.tool_use_id == id)
                    }
                    _ => None,
                })
        })
    };
    let memory: serde_json::Value =
        serde_json::from_str(&delivered("memory-source").unwrap().content)?;
    let preview = memory
        .pointer("/result/memory/preview/content")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    assert!(!preview.is_empty());
    assert!(source.starts_with(preview));
    assert_eq!(
        memory["result"]["memory"]["source_ref"],
        "agent_memory:self"
    );
    let artifact = memory["result"]["memory"]["source_artifact"]["path"]
        .as_str()
        .unwrap();
    assert_eq!(std::fs::read_to_string(artifact)?, source);
    let mut rebuilt = String::new();
    for (id, expected) in chunks {
        let result = delivered(&id).expect("range result delivered to provider");
        assert!(!result.is_error);
        let prefix = result
            .content
            .split_once("))\n")
            .expect("explicit displayed range")
            .1;
        assert_eq!(prefix, expected);
        rebuilt.push_str(prefix);
    }
    assert_eq!(rebuilt, source);
    let records = runtime.storage().read_recent_tool_executions(100)?;
    let record = records
        .iter()
        .find(|record| record.tool_name == "MemoryGet")
        .unwrap();
    assert_eq!(
        record.output["envelope"]["result"]["memory"]["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        50_000
    );
    let recovery = record.output["envelope"]["result"]["recovery_artifact"]["path"]
        .as_str()
        .unwrap();
    let canonical: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(recovery)?)?;
    assert_eq!(
        canonical["result"]["memory"]["content"],
        record.output["envelope"]["result"]["memory"]["content"]
    );
    assert!(memory["output_ref"].as_str().unwrap().contains(&record.id));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn scripted_provider_receives_all_queue_identities_without_plan_previews() -> Result<()> {
    let provider = ScriptedAgentProvider::new([
        ScriptedProviderStep::tool_use(
            "queue",
            "ListWorkItems",
            json!({"filter": "open", "limit": 22}),
        ),
        ScriptedProviderStep::text("queue inspected"),
    ]);
    let captured = provider.clone();
    let mut config = test_config();
    config.prompt_budget_estimated_tokens = 100_000;
    config.compaction_trigger_estimated_tokens = 80_000;
    let host = RuntimeHost::new_with_provider(config, Arc::new(provider))?;
    let runtime = host.default_runtime().await?;
    let mut ids = Vec::new();
    for index in 0..22 {
        let item = runtime
            .create_work_item(
                format!("Queue item {index}: {}", "long objective ".repeat(200)),
                Some(holon::types::WorkItemPlanStatus::NeedsInput),
                Some("plan detail ".repeat(1000)),
                Vec::new(),
            )
            .await?;
        ids.push(item.id);
    }
    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Inspect the open queue without activating work.".into(),
            },
        ))
        .await?;
    wait_until(|| Ok(captured.request_count() >= 2)).await?;
    let requests = captured.requests();
    let result = requests[1]
        .conversation
        .iter()
        .find_map(|message| match message {
            ConversationMessage::UserToolResults(results) => {
                results.iter().find(|result| result.tool_use_id == "queue")
            }
            _ => None,
        })
        .unwrap();
    let projected: serde_json::Value = serde_json::from_str(&result.content)?;
    assert_eq!(projected["result"]["returned"], 22);
    assert_eq!(projected["result"]["shown"], 22);
    assert_eq!(projected["result"]["omitted_count"], 0);
    let rows = projected["result"]["work_items"].as_array().unwrap();
    for id in ids {
        let row = rows.iter().find(|row| row["id"] == id).unwrap();
        assert!(row["objective"].as_str().unwrap().starts_with("Queue item"));
        assert!(row.get("plan_artifact").is_none());
        assert!(row["scheduling_state"].is_string());
    }
    let records = runtime.storage().read_recent_tool_executions(10)?;
    let record = records
        .iter()
        .find(|record| record.tool_name == "ListWorkItems")
        .unwrap();
    let canonical = &record.output["envelope"]["result"];
    assert_eq!(canonical["work_items"].as_array().unwrap().len(), 22);
    assert!(
        canonical["work_items"][0]["plan_artifact"]["preview"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            >= 1600
    );
    assert!(runtime
        .get_memory(projected["output_ref"].as_str().unwrap(), Some(1000))
        .await?
        .is_some());
    let artifact = canonical["recovery_artifact"]["path"].as_str().unwrap();
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(artifact)?)?;
    assert_eq!(saved["result"]["work_items"], canonical["work_items"]);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn scripted_agent_provider_drives_tool_loop_and_captures_requests() -> Result<()> {
    let provider = ScriptedAgentProvider::new([
        ScriptedProviderStep::tool_use("agent-get-1", "GetAgent", json!({}))
            .with_token_usage(10, 5),
        ScriptedProviderStep::text("finished after scripted tool result").with_token_usage(7, 3),
    ]);
    let captured_provider = provider.clone();
    let host = RuntimeHost::new_with_provider(test_config(), Arc::new(provider))?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "inspect agent state".into(),
            },
        ))
        .await?;

    wait_until(|| Ok(captured_provider.request_count() >= 2)).await?;
    wait_until_async(|| async {
        Ok(runtime
            .recent_briefs(10)
            .await?
            .iter()
            .any(|brief| brief.text.contains("finished after scripted tool result")))
    })
    .await?;

    let requests = captured_provider.requests();
    assert_eq!(requests.len(), 2);

    let first = &requests[0];
    let first_tool_names = first
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert!(first_tool_names.contains(&"GetAgent"));
    assert!(
        first.prompt_frame.system_prompt.contains("Use GetAgent"),
        "prompt should include GetAgent guidance when GetAgent is exposed"
    );

    let second = &requests[1];
    let tool_results = second
        .conversation
        .iter()
        .find_map(|message| match message {
            ConversationMessage::UserToolResults(results) => Some(results),
            _ => None,
        });
    let tool_results = tool_results.expect("second request should include tool results");
    let get_agent_result = tool_results
        .iter()
        .find(|result| result.tool_use_id == "agent-get-1")
        .expect("GetAgent result should be returned to the provider");
    assert!(!get_agent_result.is_error);
    let get_agent_content: serde_json::Value = serde_json::from_str(&get_agent_result.content)?;
    assert!(
        get_agent_content
            .pointer("/result/agent")
            .is_some_and(|value| value.is_object())
            || (get_agent_content
                .get("provider_projection_truncated")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
                && get_agent_content
                    .get("output_ref")
                    .is_some_and(|value| value.is_string())),
        "GetAgent tool result should preserve either the full result or its canonical truncation receipt"
    );

    let state = runtime.agent_state().await?;
    assert_eq!(state.total_model_rounds, 2);
    assert_eq!(state.total_input_tokens, 17);
    assert_eq!(state.total_output_tokens, 8);

    Ok(())
}
