// Shared runtime harness helpers.
// Reusable test helpers such as config builders, waiting helpers,
// git/worktree helpers, and tool-result parsing.

use std::path::Path;
use std::process::Command;
use std::time::Duration as StdDuration;

use anyhow::Result;
use holon::{
    config::{AppConfig, ControlAuthMode},
    provider::{ConversationMessage, ProviderTurnRequest},
    runtime::RuntimeHandle,
    tool::ToolResult,
    types::OperatorTransportBinding,
};
use tokio::time::sleep;

// Import from parent mod.rs
use super::{eventually, eventually_async, TestConfigBuilder};

/// Parse a tool result content as JSON value
pub fn parse_tool_result_value(result: &ToolResult) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(&result.content_text()?)?)
}

/// Parse the payload from a tool result
pub fn parse_tool_result_payload(result: &ToolResult) -> Result<serde_json::Value> {
    Ok(parse_tool_result_value(result)?["result"].clone())
}

/// Create a test operator transport binding for testing
pub fn operator_transport_binding(binding_id: &str, route_id: &str) -> OperatorTransportBinding {
    use chrono::Utc;
    use holon::types::OperatorTransportBindingStatus;
    use holon::types::OperatorTransportCapabilities;
    use holon::types::OperatorTransportDeliveryAuth;
    use holon::types::OperatorTransportDeliveryAuthKind;

    OperatorTransportBinding {
        binding_id: binding_id.to_string(),
        transport: "agentinbox".into(),
        operator_actor_id: "operator:jolestar".into(),
        target_agent_id: "default".into(),
        default_route_id: route_id.to_string(),
        delivery_callback_url: "http://127.0.0.1:1/delivery".into(),
        delivery_auth: OperatorTransportDeliveryAuth {
            kind: OperatorTransportDeliveryAuthKind::Bearer,
            key_id: None,
            bearer_token: Some("delivery-secret".into()),
        },
        capabilities: OperatorTransportCapabilities {
            text: true,
            markdown: None,
            attachments: None,
        },
        provider: Some("agentinbox".into()),
        provider_identity_ref: Some("agentinbox:operator:jolestar".into()),
        status: OperatorTransportBindingStatus::Active,
        created_at: Utc::now(),
        last_seen_at: None,
        metadata: None,
    }
}

/// Check if the request preserves prior tool context
pub fn preserves_prior_tool_context(request: &ProviderTurnRequest) -> bool {
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
    has_exact_tool_results || has_turn_local_recap
}

/// Create a basic test configuration
pub fn test_config() -> AppConfig {
    TestConfigBuilder::new()
        .with_control_auth_mode(ControlAuthMode::Auto)
        .build()
}

/// Create an aggressive compaction configuration for testing
pub fn aggressive_compaction_config() -> AppConfig {
    let mut config = TestConfigBuilder::new()
        .with_control_auth_mode(ControlAuthMode::Auto)
        .with_compaction(2, 1, 1, 1, 4096)
        .build();
    let override_config = holon::model_catalog::ModelRuntimeOverride {
        prompt_budget_estimated_tokens: Some(4096),
        compaction_trigger_estimated_tokens: Some(1),
        compaction_keep_recent_estimated_tokens: Some(1),
        ..holon::model_catalog::ModelRuntimeOverride::default()
    };
    config.stored_config.models.catalog.insert(
        "anthropic/claude-sonnet-4-6".into(),
        override_config.clone(),
    );
    config.validated_model_overrides.insert(
        holon::config::ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
        override_config,
    );
    config
}

/// Wait until a predicate becomes true
pub async fn wait_until(predicate: impl Fn() -> Result<bool>) -> Result<()> {
    eventually(predicate).await
}

/// Wait until an async predicate becomes true
pub async fn wait_until_async<F, Fut>(predicate: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    eventually_async(predicate).await
}

/// Wait until an async predicate becomes true with a timeout
pub async fn wait_until_async_for<F, Fut>(timeout: StdDuration, predicate: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate().await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        sleep(StdDuration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for condition"))
}

/// Get the delegated prompt text from a request
pub fn delegated_prompt_text(request: &ProviderTurnRequest) -> String {
    request
        .conversation
        .iter()
        .find_map(|message| match message {
            ConversationMessage::UserText(text) => Some(text.clone()),
            ConversationMessage::UserBlocks(blocks) => Some(
                blocks
                    .iter()
                    .map(|block| block.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

/// Run a git command in the given directory
pub fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Initialize a git repository in the given directory
pub fn init_git_repo(path: &Path) -> Result<String> {
    git(path, &["init"])?;
    git(path, &["config", "user.email", "holon@example.com"])?;
    git(path, &["config", "user.name", "Holon Test"])?;
    std::fs::write(path.join("README.md"), "holon\n")?;
    git(path, &["add", "README.md"])?;
    git(path, &["commit", "-m", "init"])?;
    git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Wait for worktree presence to match expected state
pub async fn wait_for_worktree_presence(
    runtime: &RuntimeHandle,
    expected_present: bool,
) -> Result<()> {
    for _ in 0..30 {
        let present = runtime.agent_state().await?.worktree_session.is_some();
        if present == expected_present {
            return Ok(());
        }
        sleep(StdDuration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!("timed out waiting for worktree presence"))
}

/// Snapshot of a compaction request for testing
#[derive(Debug, Clone)]
pub struct CompactionRequestSnapshot {
    pub call_index: usize,
    pub user_text_snapshot: String,
    pub assistant_text_snapshot: String,
    pub has_turn_local_recap: bool,
    pub has_full_checkpoint_request: bool,
    pub has_delta_checkpoint_request: bool,
    pub has_progress_checkpoint_request: bool,
}

/// Create a snapshot of a compaction request
pub fn compact_request_snapshot(
    call_index: usize,
    request: &ProviderTurnRequest,
) -> CompactionRequestSnapshot {
    use holon::provider::{ConversationMessage, ModelBlock};

    let user_texts: Vec<String> = request
        .conversation
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::UserText(text) => Some(text.clone()),
            ConversationMessage::UserBlocks(blocks) => Some(
                blocks
                    .iter()
                    .map(|block| block.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
        .collect();
    let assistant_texts: Vec<String> = request
        .conversation
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::AssistantBlocks(blocks) => Some(
                blocks
                    .iter()
                    .filter_map(|block| match block {
                        ModelBlock::Text { text } => Some(text.clone()),
                        ModelBlock::ToolUse { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
        .collect();
    let user_text_snapshot = user_texts.join("\n");
    let assistant_text_snapshot = assistant_texts.join("\n");

    let user_text_snapshot_lower = user_text_snapshot.to_ascii_lowercase();

    let has_turn_local_recap = user_text_snapshot_lower.contains("turn-local recap");
    let has_full_checkpoint_request = user_text_snapshot_lower.contains("full checkpoint request")
        || user_text_snapshot_lower.contains("full progress checkpoint request");
    let has_delta_checkpoint_request = user_text_snapshot_lower
        .contains("delta checkpoint request")
        || user_text_snapshot_lower.contains("delta progress checkpoint request");
    let has_progress_checkpoint_request = user_text_snapshot_lower
        .contains("progress-only checkpoint request")
        || (user_text_snapshot_lower.contains("progress checkpoint request")
            && !has_full_checkpoint_request
            && !has_delta_checkpoint_request);

    CompactionRequestSnapshot {
        call_index,
        user_text_snapshot,
        assistant_text_snapshot,
        has_turn_local_recap,
        has_full_checkpoint_request,
        has_delta_checkpoint_request,
        has_progress_checkpoint_request,
    }
}
