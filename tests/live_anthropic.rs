use anyhow::Result;
use holon::{
    config::AppConfig,
    host::RuntimeHost,
    prompt::PromptStability,
    provider::{
        AgentProvider, AnthropicProvider, ConversationMessage, ModelBlock, PromptContentBlock,
        ProviderPromptCache, ProviderPromptFrame, ProviderTurnRequest, ToolResultBlock,
    },
    tool::ToolRegistry,
    types::{
        AgentStatus, BriefKind, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        TrustLevel,
    },
};
use std::path::PathBuf;

fn live_config() -> Result<AppConfig> {
    AppConfig::load()
}

async fn wait_until_asleep(runtime: &holon::runtime::RuntimeHandle) -> Result<AgentStatus> {
    for _ in 0..300 {
        let status = runtime.agent_state().await?.status;
        if status == AgentStatus::Asleep {
            return Ok(status);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Ok(runtime.agent_state().await?.status)
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_provider_returns_real_response() -> Result<()> {
    let config = live_config()?;
    let provider = AnthropicProvider::from_config(&config)?;
    let output = provider
        .complete_turn(ProviderTurnRequest::plain(
            "Reply to the user briefly.",
            vec![ConversationMessage::UserText(
                "Reply with exactly OK.".into(),
            )],
            vec![],
        ))
        .await?;
    assert!(!output.blocks.is_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_provider_accepts_tool_result_continuation_with_runtime_tools() -> Result<()> {
    let config = live_config()?;
    let provider = AnthropicProvider::from_config(&config)?;
    let tools = ToolRegistry::new(PathBuf::from(".")).tool_specs()?;
    let context_blocks = vec![PromptContentBlock {
        text: "Agent context that should be moved into the Anthropic system prefix.".into(),
        stability: PromptStability::AgentScoped,
        cache_breakpoint: true,
    }];
    let output = provider
        .complete_turn(ProviderTurnRequest {
            prompt_frame: ProviderPromptFrame::structured(
                "Reply to the user briefly.",
                Vec::new(),
                context_blocks.clone(),
                Some(ProviderPromptCache {
                    agent_id: "live-anthropic-continuation".into(),
                    prompt_cache_key: "live-anthropic-continuation".into(),
                    working_memory_revision: 0,
                    compression_epoch: 0,
                }),
            ),
            conversation: vec![
                ConversationMessage::UserBlocks(context_blocks),
                ConversationMessage::AssistantBlocks(vec![
                    ModelBlock::Text {
                        text: "I'll start by inspecting the GitHub issue.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "call_live_issue_probe".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "gh issue view 565 --json title,body,labels,state,url"
                        }),
                    },
                ]),
                ConversationMessage::UserToolResults(vec![ToolResultBlock {
                    tool_use_id: "call_live_issue_probe".into(),
                    content: "Process exited with code 0\n\nstdout:\n{\"title\":\"Split provider contract tests into focused modules\",\"state\":\"OPEN\",\"labels\":[\"priority:p2\"],\"url\":\"https://github.com/holon-run/holon/issues/565\"}".into(),
                    is_error: false,
                    error: None,
                }]),
            ],
            tools,
        })
        .await?;
    assert!(!output.blocks.is_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires real provider credentials and network access"]
async fn live_runtime_wakes_sleeps_and_preserves_context() -> Result<()> {
    let mut config = live_config()?;
    config.data_dir = tempfile::tempdir()?.keep();
    let host = RuntimeHost::new(config.clone())?;
    let runtime = host.default_runtime().await?;

    runtime
        .enqueue(MessageEnvelope::new(
            &config.default_agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Reply with exactly FIRST_OK.".into(),
            },
        ))
        .await?;
    let first_status = wait_until_asleep(&runtime).await?;
    assert_eq!(first_status, AgentStatus::Asleep);

    runtime
        .enqueue(MessageEnvelope::new(
            &config.default_agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Tell me what the previous result was in one short sentence.".into(),
            },
        ))
        .await?;
    let final_status = wait_until_asleep(&runtime).await?;

    let briefs = runtime.recent_briefs(10).await?;
    let result_briefs = briefs
        .iter()
        .filter(|brief| brief.kind == BriefKind::Result)
        .collect::<Vec<_>>();
    assert!(
        result_briefs.len() >= 2,
        "expected at least two result briefs, got {:?}",
        briefs
            .iter()
            .map(|brief| format!("{:?}: {}", brief.kind, brief.text))
            .collect::<Vec<_>>()
    );
    let second_result = result_briefs.last().unwrap().text.to_lowercase();
    assert!(
        second_result.contains("first")
            || second_result.contains("ok")
            || second_result.contains("previous result")
            || second_result.contains("exact requested phrase")
            || second_result.contains("replied with"),
        "unexpected final brief: {}\nall briefs: {:?}",
        result_briefs.last().unwrap().text,
        briefs
            .iter()
            .map(|brief| brief.text.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(final_status, AgentStatus::Asleep);
    Ok(())
}
