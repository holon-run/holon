use std::{fs, path::Path, process::Command, time::Duration};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use holon::{
    config::{AppConfig, ProviderId, ProviderRuntimeConfig},
    provider::{
        AgentProvider, ConversationMessage, OpenAiCodexProvider, ProviderPromptCache,
        ProviderPromptFrame, ProviderTurnRequest,
    },
};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

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

fn codex_responses_route(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    let api_base = if base_url.ends_with("/codex") {
        base_url.to_string()
    } else {
        format!("{base_url}/codex")
    };
    format!("{api_base}/responses")
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
    if diagnostics.status != "compacted" || diagnostics.compaction_items.unwrap_or(0) == 0 {
        anyhow::bail!(
            "openai-codex compact route did not return a usable compaction item: status={}, compaction_items={:?}",
            diagnostics.status,
            diagnostics.compaction_items
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires Codex CLI ChatGPT auth and network access"]
async fn live_openai_codex_request_control_probe() -> Result<()> {
    let config = live_config()?;
    let provider_config = config
        .providers
        .get(&ProviderId::openai_codex())
        .ok_or_else(|| anyhow!("missing openai-codex provider config"))?;
    let credential = load_live_codex_credential(provider_config)?;
    let route = codex_responses_route(&provider_config.base_url);
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build Codex probe HTTP client")?;

    let default =
        post_codex_probe(&client, &route, &credential, codex_probe_body(None, false)).await?;
    eprintln!(
        "openai-codex request controls: default status={}, body_hint={}",
        default.status,
        body_hint(&default.body)
    );
    assert!(
        (200..300).contains(&default.status),
        "default request should be accepted: {}",
        body_hint(&default.body)
    );

    let low = post_codex_probe(
        &client,
        &route,
        &credential,
        codex_probe_body(Some("low"), false),
    )
    .await?;
    eprintln!(
        "openai-codex request controls: reasoning_low status={}, body_hint={}",
        low.status,
        body_hint(&low.body)
    );
    assert!(
        (200..300).contains(&low.status),
        "reasoning effort low should be accepted: {}",
        body_hint(&low.body)
    );

    let max_output = post_codex_probe(
        &client,
        &route,
        &credential,
        codex_probe_body(Some("low"), true),
    )
    .await?;
    eprintln!(
        "openai-codex request controls: max_output_tokens status={}, body_hint={}",
        max_output.status,
        body_hint(&max_output.body)
    );
    assert_eq!(
        max_output.status, 400,
        "max_output_tokens should remain unsupported by the Codex backend"
    );
    assert!(
        max_output.body.contains("max_output_tokens")
            || max_output.body.contains("Unsupported parameter"),
        "unexpected max_output_tokens error body: {}",
        body_hint(&max_output.body)
    );

    Ok(())
}

#[derive(Debug, Deserialize)]
struct AuthDotJson {
    tokens: Option<TokenData>,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    access_token: String,
    account_id: Option<String>,
    id_token: Option<Value>,
}

#[derive(Debug)]
struct LiveCodexCredential {
    access_token: String,
    account_id: String,
}

#[derive(Debug)]
struct ProbeResponse {
    status: u16,
    body: String,
}

fn load_live_codex_credential(
    provider_config: &ProviderRuntimeConfig,
) -> Result<LiveCodexCredential> {
    let codex_home = provider_config
        .codex_home
        .as_deref()
        .ok_or_else(|| anyhow!("missing codex_home for OpenAI Codex provider"))?;
    let codex_home = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    let auth = load_live_codex_auth_from_file(&codex_home).or_else(|file_error| {
        load_live_codex_auth_from_keychain(&codex_home).with_context(|| {
            format!("failed to load Codex auth from auth.json or keychain: {file_error}")
        })
    })?;
    let tokens = auth
        .tokens
        .ok_or_else(|| anyhow!("Codex auth record did not contain OAuth tokens"))?;
    let account_id = tokens
        .account_id
        .or_else(|| {
            tokens
                .id_token
                .as_ref()
                .and_then(extract_account_id_from_id_token)
        })
        .or_else(|| extract_account_id_from_access_token(&tokens.access_token))
        .ok_or_else(|| anyhow!("failed to resolve Codex account id from stored credentials"))?;
    Ok(LiveCodexCredential {
        access_token: tokens.access_token,
        account_id,
    })
}

fn load_live_codex_auth_from_file(codex_home: &Path) -> Result<AuthDotJson> {
    let path = codex_home.join("auth.json");
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

fn load_live_codex_auth_from_keychain(codex_home: &Path) -> Result<AuthDotJson> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("Codex keychain auth fallback is only available on macOS");
    }
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Codex Auth",
            "-a",
            &compute_codex_keychain_account(codex_home),
            "-w",
        ])
        .output()
        .context("failed to invoke macOS security tool")?;
    if !output.status.success() {
        anyhow::bail!("macOS keychain does not contain a Codex Auth entry");
    }
    let secret = String::from_utf8(output.stdout).context("Codex keychain entry was not UTF-8")?;
    serde_json::from_str(secret.trim()).context("failed to parse Codex keychain auth record")
}

fn compute_codex_keychain_account(codex_home: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(codex_home.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    format!("cli|{}", &hex[..16])
}

fn codex_probe_body(reasoning_effort: Option<&str>, include_max_output_tokens: bool) -> Value {
    let mut body = json!({
        "model": live_openai_codex_model(),
        "instructions": "Reply with exactly one short sentence.",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "Say compact probe ok." }]
        }],
        "tools": [],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
        "stream": true,
        "reasoning": Value::Null,
        "include": [],
    });
    if let Some(reasoning_effort) = reasoning_effort {
        body["reasoning"] = json!({ "effort": reasoning_effort });
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if include_max_output_tokens {
        body["max_output_tokens"] = json!(32);
    }
    body
}

async fn post_codex_probe(
    client: &Client,
    route: &str,
    credential: &LiveCodexCredential,
    body: Value,
) -> Result<ProbeResponse> {
    let response = client
        .post(route)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!("Bearer {}", credential.access_token),
        )
        .header("chatgpt-account-id", &credential.account_id)
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "codex_cli_rs")
        .json(&body)
        .send()
        .await
        .context("failed to send Codex probe request")?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .context("failed to read Codex probe response")?;
    Ok(ProbeResponse { status, body })
}

fn extract_account_id_from_access_token(token: &str) -> Option<String> {
    let payload = decode_jwt_payload(token)?;
    extract_account_id_from_payload(&payload)
}

fn extract_account_id_from_id_token(id_token: &Value) -> Option<String> {
    match id_token {
        Value::String(jwt) => {
            decode_jwt_payload(jwt).and_then(|payload| extract_account_id_from_payload(&payload))
        }
        Value::Object(payload) => extract_account_id_from_payload(&Value::Object(payload.clone())),
        _ => None,
    }
}

fn decode_jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn extract_account_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("account_id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("chatgpt_account_id").and_then(Value::as_str))
        .or_else(|| {
            payload
                .get("https://api.openai.com/auth")
                .and_then(|value| value.get("chatgpt_account_id"))
                .and_then(Value::as_str)
        })
        .map(ToString::to_string)
}

fn body_hint(body: &str) -> String {
    body.chars().take(240).collect()
}
