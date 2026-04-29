use std::{fs, path::PathBuf, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use holon::{
    provider::{
        AgentProvider, ConversationMessage, ModelBlock, ProviderTurnRequest, ProviderTurnResponse,
    },
    types::{MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, TrustLevel},
};
use serde::Deserialize;
use tempfile::tempdir;
use tokio::sync::Mutex;
mod support;

use support::{eventually, RuntimeHarness, TestConfigBuilder};

#[derive(Debug, Deserialize)]
struct RegressionFixture {
    prompt: String,
    steps: Vec<FixtureStep>,
    expected_file: String,
    expected_content: String,
    expected_brief: String,
}

#[derive(Debug, Clone, Deserialize)]
struct FixtureStep {
    blocks: Vec<ModelBlock>,
}

struct FixtureProvider {
    steps: Vec<FixtureStep>,
    index: Mutex<usize>,
}

impl FixtureProvider {
    fn new(steps: Vec<FixtureStep>) -> Self {
        Self {
            steps,
            index: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AgentProvider for FixtureProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut index = self.index.lock().await;
        let step = self.steps.get(*index).cloned().unwrap_or(FixtureStep {
            blocks: vec![ModelBlock::Text {
                text: "fixture exhausted".into(),
            }],
        });
        *index += 1;

        if *index > 1 {
            let has_exact_tool_results = request
                .conversation
                .iter()
                .any(|message| matches!(message, ConversationMessage::UserToolResults(_)));
            let has_turn_local_recap = request.conversation.iter().any(|message| {
                matches!(
                    message,
                    ConversationMessage::UserText(text)
                        if text.contains("Turn-local recap for older completed rounds")
                )
            });
            assert!(
                has_exact_tool_results || has_turn_local_recap,
                "continuation request should preserve prior tool context via exact tool results or a turn-local recap: {:?}",
                request.conversation
            );
        }

        Ok(ProviderTurnResponse {
            blocks: step.blocks,
            stop_reason: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            request_diagnostics: None,
        })
    }
}
#[tokio::test]
async fn fixture_coding_loop_regression_stays_green() -> Result<()> {
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/write_and_verify.json");
    let fixture: RegressionFixture = serde_json::from_str(&fs::read_to_string(&fixture_path)?)?;

    let workspace = tempdir()?.keep();
    let data_dir = tempdir()?.keep();
    fs::create_dir_all(&workspace)?;
    let provider = FixtureProvider::new(fixture.steps.clone());

    let harness = RuntimeHarness::with_config_and_provider(
        TestConfigBuilder::new()
            .with_workspace_dir(workspace.clone())
            .with_data_dir(data_dir)
            .build(),
        Arc::new(provider),
    )
    .await?;
    let runtime = harness.runtime.clone();

    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: fixture.prompt,
            },
        ))
        .await?;

    let expected_path = workspace.join(&fixture.expected_file);
    eventually(|| Ok(expected_path.exists())).await?;

    let content = fs::read_to_string(expected_path)?;
    assert_eq!(content, fixture.expected_content);

    eventually(|| {
        let briefs = runtime.storage().read_recent_briefs(10)?;
        Ok(briefs
            .iter()
            .any(|brief| brief.text.contains(&fixture.expected_brief)))
    })
    .await?;
    Ok(())
}
