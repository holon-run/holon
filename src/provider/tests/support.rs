//! Shared test fixtures and helper functions for provider contract tests.

use super::*;
use crate::{
    config::{provider_registry_for_tests, AppConfig, ControlAuthMode, ModelRef},
    prompt::PromptStability,
    tool::{ToolRegistry, ToolSpec},
};
use axum::Router;
use base64::Engine;
use serde_json::json;
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub struct ProviderTestFixture {
    pub _home_dir: tempfile::TempDir,
    pub _workspace_dir: tempfile::TempDir,
    pub config: AppConfig,
}

pub async fn spawn_test_server(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{}", addr)
}

#[allow(dead_code)]
pub async fn spawn_raw_http_server(response: &'static [u8]) -> String {
    spawn_raw_http_server_sequence(vec![response]).await
}

pub async fn spawn_raw_http_server_sequence(responses: Vec<&'static [u8]>) -> String {
    spawn_raw_http_server_bytes_sequence(
        responses
            .into_iter()
            .map(|response| response.to_vec())
            .collect(),
    )
    .await
}

pub async fn spawn_raw_http_server_bytes_sequence(responses: Vec<Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            drain_http_request(&mut stream).await;
            stream.write_all(&response).await.unwrap();
        }
    });
    format!("http://{}", addr)
}

async fn drain_http_request(stream: &mut TcpStream) {
    let mut buffer = [0u8; 1024];
    let mut request = Vec::new();
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers_end = headers_end + 4;
            let content_length = parse_content_length(&request[..headers_end]).unwrap_or(0);
            while request.len() < headers_end + content_length {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            break;
        }
    }
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
    std::str::from_utf8(headers).ok()?.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    })
}

pub fn provider_turn_request() -> ProviderTurnRequest {
    ProviderTurnRequest::plain(
        "You are a coding agent",
        vec![ConversationMessage::UserText("hello".into())],
        Vec::new(),
    )
}

pub fn provider_turn_request_with_tools(tools: Vec<ToolSpec>) -> ProviderTurnRequest {
    ProviderTurnRequest::plain(
        "You are a coding agent",
        vec![ConversationMessage::UserText("hello".into())],
        tools,
    )
}

pub fn provider_turn_request_with_prompt_frame() -> ProviderTurnRequest {
    ProviderTurnRequest {
        prompt_frame: ProviderPromptFrame::structured(
            "rendered system",
            vec![PromptContentBlock {
                text: "stable system".into(),
                stability: PromptStability::Stable,
                cache_breakpoint: true,
            }],
            vec![PromptContentBlock {
                text: "agent context".into(),
                stability: PromptStability::AgentScoped,
                cache_breakpoint: true,
            }],
            Some(ProviderPromptCache {
                agent_id: "default".into(),
                prompt_cache_key: "cache-key".into(),
                working_memory_revision: 7,
                compression_epoch: 3,
            }),
        ),
        conversation: vec![ConversationMessage::UserBlocks(vec![PromptContentBlock {
            text: "agent context".into(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: true,
        }])],
        tools: Vec::new(),
    }
}

pub fn provider_continuation_request_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_turn_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-1".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
        }]),
        ConversationMessage::UserToolResults(vec![ToolResultBlock {
            tool_use_id: "exec-1".into(),
            content: "ok".into(),
            is_error: false,
            error: None,
        }]),
    ]);
    request
}

pub fn sleep_tool_spec() -> ToolSpec {
    tool_spec_named("Sleep")
}

pub fn trusted_tool_specs() -> Vec<ToolSpec> {
    ToolRegistry::new(std::path::PathBuf::from("."))
        .tool_specs()
        .unwrap()
}

pub fn tool_spec_named(name: &str) -> ToolSpec {
    ToolRegistry::new(std::path::PathBuf::from("."))
        .tool_specs()
        .unwrap()
        .into_iter()
        .find(|spec| spec.name == name)
        .unwrap_or_else(|| panic!("{name} tool should be present"))
}

fn encode_segment(value: serde_json::Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
}

fn fake_jwt(payload: serde_json::Value) -> String {
    format!(
        "{}.{}.{}",
        encode_segment(json!({"alg": "none"})),
        encode_segment(payload),
        encode_segment(json!("sig"))
    )
}

fn write_codex_auth(codex_home: &std::path::Path) {
    std::fs::create_dir_all(codex_home).unwrap();
    let token = fake_jwt(json!({
        "account_id": "acct_test",
        "exp": 4_102_444_800i64
    }));
    std::fs::write(
        codex_home.join("auth.json"),
        json!({
            "tokens": {
                "access_token": token,
                "refresh_token": "refresh-token",
                "account_id": "acct_test"
            }
        })
        .to_string(),
    )
    .unwrap();
}

pub fn test_config(
    default_model: &str,
    fallback_models: &[&str],
    openai_key: Option<&str>,
    anthropic_token: Option<&str>,
    with_codex_auth: bool,
) -> ProviderTestFixture {
    let home_dir = tempdir().unwrap();
    let workspace_dir = tempdir().unwrap();
    let home_path = home_dir.path().to_path_buf();
    let workspace_path = workspace_dir.path().to_path_buf();
    let codex_home = home_path.join(".codex");
    if with_codex_auth {
        write_codex_auth(&codex_home);
    }
    let config = AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
        home_dir: home_path.clone(),
        data_dir: home_path.clone(),
        socket_path: home_path.join("run").join("holon.sock"),
        workspace_dir: workspace_path,
        context_window_messages: 8,
        context_window_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens: 4096,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        control_token: Some("secret".into()),
        control_auth_mode: ControlAuthMode::Auto,
        config_file_path: home_path.join("config.json"),
        stored_config: Default::default(),
        default_model: ModelRef::parse(default_model).unwrap(),
        fallback_models: fallback_models
            .iter()
            .map(|value| ModelRef::parse(value).unwrap())
            .collect(),
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
        max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
        disable_provider_fallback: false,
        tui_alternate_screen: crate::config::AltScreenMode::Auto,
        validated_model_overrides: std::collections::HashMap::new(),
        validated_unknown_model_fallback: None,
        providers: provider_registry_for_tests(openai_key, anthropic_token, codex_home),
    };
    ProviderTestFixture {
        _home_dir: home_dir,
        _workspace_dir: workspace_dir,
        config,
    }
}
