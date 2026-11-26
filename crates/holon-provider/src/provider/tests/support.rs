use super::*;
use crate::config::{
    CredentialKind, CredentialSource, ProviderAuthConfig, ProviderEndpointId, ProviderId,
    ProviderRuntimeConfig, ProviderTransportKind, OPENAI_CODEX_CREDENTIAL_PROFILE,
};
use axum::Router;
use base64::Engine;
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub const TEST_CLIENT_VERSION: &str = "0.0.0-test";
pub const TEST_OUTPUT_TOKENS: u32 = 8192;

pub struct ProviderFixture {
    pub home: tempfile::TempDir,
    pub config: ProviderRuntimeConfig,
}

impl ProviderFixture {
    pub fn openai(credential: &str) -> Self {
        Self::new(
            ProviderId::openai(),
            ProviderTransportKind::OpenAiResponses,
            credential,
        )
    }

    pub fn anthropic(credential: &str) -> Self {
        Self::new(
            ProviderId::anthropic(),
            ProviderTransportKind::AnthropicMessages,
            credential,
        )
    }

    pub fn gemini(credential: &str) -> Self {
        Self::new(
            ProviderId::gemini(),
            ProviderTransportKind::GeminiGenerateContent,
            credential,
        )
    }

    pub fn codex() -> Self {
        let mut fixture = Self::new(
            ProviderId::openai_codex(),
            ProviderTransportKind::OpenAiCodexResponses,
            "",
        );
        let codex_home = fixture.home.path().join(".codex");
        write_codex_auth(&codex_home);
        fixture.config.credential = None;
        fixture.config.codex_home = Some(codex_home);
        fixture.config.auth.source = CredentialSource::AuthProfile;
        fixture.config.auth.kind = CredentialKind::OAuth;
        fixture.config.auth.profile = Some(OPENAI_CODEX_CREDENTIAL_PROFILE.into());
        fixture.config.auth.external = Some("codex_cli".into());
        fixture.config.originator = Some("codex_cli_rs".into());
        fixture.config.reasoning_effort = Some("low".into());
        fixture
    }

    fn new(id: ProviderId, transport: ProviderTransportKind, credential: &str) -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            config: ProviderRuntimeConfig {
                route_provider: id.clone(),
                id,
                route_endpoint: ProviderEndpointId::default_endpoint(),
                transport,
                base_url: "http://127.0.0.1:0".into(),
                auth: ProviderAuthConfig {
                    source: CredentialSource::Env,
                    kind: CredentialKind::ApiKey,
                    ..Default::default()
                },
                headers: Default::default(),
                credential: Some(credential.into()),
                credential_store_path: None,
                codex_home: None,
                originator: None,
                reasoning_effort: None,
                context_management: Default::default(),
                builtin_web_search: None,
            },
        }
    }
}

pub fn anthropic_provider(fixture: &ProviderFixture) -> AnthropicProvider {
    AnthropicProvider::from_runtime_config(
        &fixture.config,
        "claude-sonnet-4-6",
        TEST_OUTPUT_TOKENS,
        fixture.home.path(),
        true,
        TEST_CLIENT_VERSION,
    )
    .unwrap()
}

pub async fn spawn_test_server(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{}", addr)
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

pub async fn spawn_raw_http_server_scripted(responses: Vec<Vec<(u64, Vec<u8>)>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for chunks in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            drain_http_request(&mut stream).await;
            for (delay_ms, chunk) in chunks {
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                stream.write_all(&chunk).await.unwrap();
            }
        }
    });
    format!("http://{}", addr)
}

pub fn chunked_http_response(chunks: &[&[u8]]) -> Vec<u8> {
    let mut response = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".to_vec();
    for chunk in chunks {
        response.extend_from_slice(format!("{:X}\r\n", chunk.len()).as_bytes());
        response.extend_from_slice(chunk);
        response.extend_from_slice(b"\r\n");
    }
    response.extend_from_slice(b"0\r\n\r\n");
    response
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
        dispatch_guard: None,
        provider_session_id: None,
        material: Default::default(),
        continuation_scope_id: ContinuationScopeId::new("default"),
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
                context_fingerprint: "fingerprint-cache-key".into(),
                compression_epoch: 3,
            }),
        ),
        conversation: vec![ConversationMessage::UserBlocks(vec![PromptContentBlock {
            text: "agent context".into(),
            stability: PromptStability::AgentScoped,
            cache_breakpoint: true,
        }])],
        tools: Vec::new(),
        native_web_search: None,
        response_format: None,
    }
}

pub fn provider_continuation_request_with_prompt_frame() -> ProviderTurnRequest {
    let mut request = provider_turn_request_with_prompt_frame();
    request.conversation.extend([
        ConversationMessage::AssistantBlocks(vec![ModelBlock::ToolUse {
            id: "exec-1".into(),
            name: "ExecCommand".into(),
            input: json!({ "cmd": "printf ok" }),
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
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

pub fn write_codex_auth(codex_home: &std::path::Path) {
    write_codex_auth_with_exp(codex_home, 4_102_444_800i64);
}

pub fn write_codex_auth_with_exp(codex_home: &std::path::Path, exp: i64) {
    std::fs::create_dir_all(codex_home).unwrap();
    let token = fake_jwt(json!({
        "account_id": "acct_test",
        "exp": exp
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

pub fn responses_provider(fixture: &ProviderFixture, trigger_input_tokens: u64) -> OpenAiProvider {
    OpenAiProvider::from_runtime_config_with_compaction_policy_and_context_window(
        &fixture.config,
        "gpt-5.4",
        TEST_OUTPUT_TOKENS,
        None,
        fixture.home.path(),
        crate::provider::transports::OpenAiCompactionPolicy {
            trigger_input_tokens,
        },
        TEST_CLIENT_VERSION,
    )
    .unwrap()
}

pub fn codex_provider(fixture: &ProviderFixture, trigger_input_tokens: u64) -> OpenAiCodexProvider {
    OpenAiCodexProvider::from_runtime_config_with_compaction_policy_and_context_window(
        &fixture.config,
        "gpt-5.4",
        TEST_OUTPUT_TOKENS,
        None,
        fixture.home.path(),
        crate::provider::transports::OpenAiCompactionPolicy {
            trigger_input_tokens,
        },
        Some(holon_domain::model_values::ModelVerbosity::Low),
        true,
        TEST_CLIENT_VERSION,
    )
    .unwrap()
}

pub fn single_route(
    provider_name: &str,
    model: &str,
    provider: Arc<dyn AgentProvider>,
) -> FallbackProvider {
    FallbackProvider::new(
        vec![ProviderCandidate {
            model_ref: format!("{provider_name}@default/{model}"),
            provider_name: provider_name.into(),
            transport_id: None,
            resolved_image_input: true,
            provider,
        }],
        |_| {},
    )
}
