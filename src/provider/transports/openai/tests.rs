use super::{
    build_openai_codex_image_generation_request, build_openai_responses_request,
    chat_completions_url, choose_openai_codex_credential, consume_openai_sse_event,
    incremental_diagnostics, latest_openai_compaction_index, native_web_search_diagnostics,
    openai_compaction_trigger_for_request_plan, openai_compaction_trigger_for_window,
    openai_model_policy_for_runtime_config, openai_provider_window_compaction_candidate,
    parse_openai_codex_image_generation_response_items, plan_openai_responses_request,
    resolve_openai_codex_credential, CredentialStoreRefreshLock, OpenAiChatCompletionsProvider,
    OpenAiCodexProvider, OpenAiCompactionPolicy, OpenAiContinuationState, OpenAiProvider,
    OpenAiProviderWindow, OpenAiRequestPlan, OpenAiRequestShape,
    OpenAiResponsesContinuationContract, OpenAiResponsesTransportContract, ToolSchemaContract,
};
use crate::auth::CodexCliCredential;
use crate::config::{
    load_credential_store_at, save_credential_store_at, CredentialKind, CredentialProfileFile,
    CredentialSource, CredentialStoreFile, ProviderAuthConfig, ProviderEndpointId, ProviderId,
    ProviderRuntimeConfig, ProviderTransportKind, OPENAI_CODEX_CREDENTIAL_PROFILE,
};
use crate::provider::retry::{classify_provider_error, ProviderFailureKind, RetryDisposition};
use crate::provider::{
    ConversationMessage, ProviderGenerateImageRequest, ProviderJsonSchemaResponseFormat,
    ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest, ProviderResponseFormatRequest,
    ProviderTurnRequest,
};
use base64::prelude::BASE64_STANDARD;
use base64::Engine;
use chrono::Utc;
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

static CODEX_REFRESH_ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_ref() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn encode_segment(value: serde_json::Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
}

fn make_token(payload: serde_json::Value) -> String {
    format!(
        "{}.{}.{}",
        encode_segment(json!({"alg": "none"})),
        encode_segment(payload),
        encode_segment(json!("sig"))
    )
}

fn test_openai_codex_config(credential: Option<String>) -> ProviderRuntimeConfig {
    ProviderRuntimeConfig {
        id: ProviderId::openai_codex(),
        route_provider: ProviderId::openai_codex(),
        route_endpoint: ProviderEndpointId::default_endpoint(),
        transport: ProviderTransportKind::OpenAiCodexResponses,
        base_url: "https://chatgpt.com/backend-api/codex".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::AuthProfile,
            kind: CredentialKind::OAuth,
            env: None,
            profile: Some(OPENAI_CODEX_CREDENTIAL_PROFILE.into()),
            external: Some("codex_cli".into()),
        },
        credential,
        credential_store_path: None,
        codex_home: Some(PathBuf::from("/tmp/codex-home")),
        originator: Some("codex_cli_rs".into()),
        reasoning_effort: Some("low".into()),
        context_management: Default::default(),
        builtin_web_search: None,
    }
}

fn test_xai_oauth_config(credential: String) -> ProviderRuntimeConfig {
    let xai = ProviderId::parse("xai").unwrap();
    ProviderRuntimeConfig {
        id: xai.clone(),
        route_provider: xai,
        route_endpoint: ProviderEndpointId::default_endpoint(),
        transport: ProviderTransportKind::OpenAiResponses,
        base_url: "https://api.x.ai/v1".into(),
        auth: ProviderAuthConfig {
            source: CredentialSource::AuthProfile,
            kind: CredentialKind::OAuth,
            env: None,
            profile: Some("xai".into()),
            external: None,
        },
        credential: Some(credential),
        credential_store_path: None,
        codex_home: None,
        originator: None,
        reasoning_effort: Some("medium".into()),
        context_management: Default::default(),
        builtin_web_search: None,
    }
}

#[test]
fn runtime_config_policy_preserves_exact_route_and_explicit_output_limit() {
    let mut config = test_openai_codex_config(Some("credential".into()));
    config.route_provider = ProviderId::parse("volcengine").unwrap();
    config.route_endpoint = ProviderEndpointId::parse("plan").unwrap();

    let clamped = openai_model_policy_for_runtime_config(&config, "glm-5.2", 200_000);
    assert_eq!(clamped.runtime_max_output_tokens, 128_000);
    assert_eq!(clamped.reasoning_effort_options, ["low", "medium", "high"]);

    let explicit = openai_model_policy_for_runtime_config(&config, "glm-5.2", 1_024);
    assert_eq!(explicit.runtime_max_output_tokens, 1_024);
}

#[test]
fn chat_completions_resolved_runtime_config_does_not_resolve_metadata_again() {
    let mut config = test_openai_codex_config(Some("credential".into()));
    config.route_provider = ProviderId::parse("volcengine").unwrap();
    config.route_endpoint = ProviderEndpointId::parse("plan").unwrap();
    let home = tempfile::tempdir().unwrap();

    let provider = OpenAiChatCompletionsProvider::from_resolved_runtime_config(
        &config,
        "glm-5.2",
        200_000,
        home.path(),
    )
    .unwrap();

    assert_eq!(provider.max_output_tokens, 200_000);
}

#[test]
fn chat_completions_url_accepts_openai_compatible_base_urls() {
    assert_eq!(
        chat_completions_url("https://api.deepseek.com"),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://openrouter.ai/api/v1"),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://qianfan.baidubce.com/v2"),
        "https://qianfan.baidubce.com/v2/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://ark.ap-southeast.bytepluses.com/api/v3"),
        "https://ark.ap-southeast.bytepluses.com/api/v3/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://api.z.ai/api/paas/v4"),
        "https://api.z.ai/api/paas/v4/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://proxy.example/chat/completions"),
        "https://proxy.example/chat/completions"
    );
}

#[tokio::test]
async fn openai_provider_uses_xai_oauth_profile_as_bearer_token() {
    let access_token = make_token(json!({
        "exp": Utc::now().timestamp() + 7200
    }));
    let material = json!({
        "tokens": {
            "access_token": access_token,
            "refresh_token": "refresh"
        }
    })
    .to_string();
    let config = test_xai_oauth_config(material);
    let home = tempfile::tempdir().unwrap();
    let provider =
        OpenAiProvider::from_runtime_config(&config, "grok-test", 1024, home.path()).unwrap();

    let headers = provider.resolve_auth_headers().await.unwrap();

    assert_eq!(
        headers,
        vec![("authorization", format!("Bearer {access_token}"))]
    );
}

#[test]
fn openai_codex_resolves_holon_oauth_profile_before_cli_files() {
    let credential_material = json!({
        "tokens": {
            "access_token": make_token(json!({
                "exp": 1_900_000_000,
                "chatgpt_account_id": "acct_profile"
            })),
            "refresh_token": "refresh",
            "account_id": "acct_profile"
        }
    })
    .to_string();
    let provider_config = test_openai_codex_config(Some(credential_material));

    let credential = resolve_openai_codex_credential(
        &provider_config,
        provider_config.codex_home.as_ref().unwrap(),
    )
    .expect("Holon profile credential should resolve");

    assert_eq!(credential.account_id, "acct_profile");
    assert_eq!(
        credential.source,
        format!("credential_profile:{OPENAI_CODEX_CREDENTIAL_PROFILE}")
    );
}

#[test]
fn openai_codex_prefers_profile_over_fresher_cli_credential() {
    let profile = CodexCliCredential {
        access_token: "profile-access".into(),
        account_id: "acct_profile".into(),
        expires_at: chrono::DateTime::from_timestamp(1_900_000_000, 0),
        refreshed_at: chrono::DateTime::from_timestamp(1_800_000_000, 0),
        source: format!("credential_profile:{OPENAI_CODEX_CREDENTIAL_PROFILE}"),
    };
    let cli = CodexCliCredential {
        access_token: "cli-access".into(),
        account_id: "acct_cli".into(),
        expires_at: chrono::DateTime::from_timestamp(1_910_000_000, 0),
        refreshed_at: chrono::DateTime::from_timestamp(1_810_000_000, 0),
        source: "keychain".into(),
    };

    let credential = choose_openai_codex_credential(Some(profile), Some(cli))
        .expect("credential should resolve");

    assert_eq!(credential.access_token, "profile-access");
    assert_eq!(
        credential.source,
        format!("credential_profile:{OPENAI_CODEX_CREDENTIAL_PROFILE}")
    );
}

#[tokio::test]
async fn openai_codex_refreshes_and_persists_holon_oauth_profile() {
    let server = axum::Router::new().route(
        "/oauth/token",
        axum::routing::post(|| async {
            axum::Json(json!({
                "access_token": make_token(json!({
                    "exp": 1_900_000_000,
                    "chatgpt_account_id": "acct_profile"
                })),
                "refresh_token": "rotated-refresh"
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
    let _env = EnvVarGuard::set(
        "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
        format!("http://{addr}/oauth/token"),
    );

    let home = tempfile::tempdir().unwrap();
    let credential_store_path = home.path().join("credentials.json");
    let expiring_material = json!({
        "tokens": {
            "access_token": make_token(json!({
                "exp": Utc::now().timestamp() + 30,
                "chatgpt_account_id": "acct_profile"
            })),
            "refresh_token": "old-refresh",
            "account_id": "acct_profile"
        }
    })
    .to_string();
    let mut profiles = BTreeMap::new();
    profiles.insert(
        OPENAI_CODEX_CREDENTIAL_PROFILE.to_string(),
        CredentialProfileFile {
            kind: CredentialKind::OAuth,
            material: expiring_material.clone(),
        },
    );
    save_credential_store_at(&credential_store_path, &CredentialStoreFile { profiles }).unwrap();

    let mut provider_config = test_openai_codex_config(Some(expiring_material));
    provider_config.credential_store_path = Some(credential_store_path.clone());
    let provider = OpenAiCodexProvider::from_runtime_config(
        &provider_config,
        "gpt-codex-test",
        1024,
        home.path(),
        true,
    )
    .unwrap();

    let credential = provider.resolve_fresh_credential().await.unwrap();
    assert_eq!(credential.account_id, "acct_profile");

    let store = load_credential_store_at(&credential_store_path).unwrap();
    let material = &store.profiles[OPENAI_CODEX_CREDENTIAL_PROFILE].material;
    assert!(material.contains("rotated-refresh"));
    assert!(!material.contains("old-refresh"));
    handle.abort();
}

#[tokio::test]
async fn openai_codex_falls_back_to_cli_credential_when_profile_refresh_fails() {
    let server = axum::Router::new().route(
        "/oauth/token",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::UNAUTHORIZED,
                axum::Json(json!({
                    "error": "invalid_grant",
                    "error_description": "refresh token expired"
                })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
    let _env = EnvVarGuard::set(
        "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
        format!("http://{addr}/oauth/token"),
    );

    let home = tempfile::tempdir().unwrap();
    let codex_home = home.path().join("codex-home");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(
        codex_home.join("auth.json"),
        json!({
            "tokens": {
                "access_token": make_token(json!({
                    "exp": Utc::now().timestamp() + 3600,
                    "chatgpt_account_id": "acct_cli"
                })),
                "refresh_token": "cli-refresh",
                "account_id": "acct_cli"
            },
            "last_refresh": Utc::now()
        })
        .to_string(),
    )
    .unwrap();

    let credential_store_path = home.path().join("credentials.json");
    let expiring_material = json!({
        "tokens": {
            "access_token": make_token(json!({
                "exp": Utc::now().timestamp() + 30,
                "chatgpt_account_id": "acct_profile"
            })),
            "refresh_token": "expired-refresh",
            "account_id": "acct_profile"
        }
    })
    .to_string();
    let mut profiles = BTreeMap::new();
    profiles.insert(
        OPENAI_CODEX_CREDENTIAL_PROFILE.to_string(),
        CredentialProfileFile {
            kind: CredentialKind::OAuth,
            material: expiring_material.clone(),
        },
    );
    save_credential_store_at(&credential_store_path, &CredentialStoreFile { profiles }).unwrap();

    let mut provider_config = test_openai_codex_config(Some(expiring_material));
    provider_config.credential_store_path = Some(credential_store_path);
    provider_config.codex_home = Some(codex_home);
    let provider = OpenAiCodexProvider::from_runtime_config(
        &provider_config,
        "gpt-codex-test",
        1024,
        home.path(),
        true,
    )
    .unwrap();

    let credential = provider.resolve_fresh_credential().await.unwrap();

    assert_eq!(credential.account_id, "acct_cli");
    assert_eq!(credential.source, "file");
    handle.abort();
}

#[tokio::test]
async fn openai_codex_auth_failure_forces_profile_refresh_even_when_jwt_is_not_expiring() {
    let server = axum::Router::new().route(
        "/oauth/token",
        axum::routing::post(|| async {
            axum::Json(json!({
                "access_token": make_token(json!({
                    "exp": Utc::now().timestamp() + 3600,
                    "chatgpt_account_id": "acct_profile"
                })),
                "refresh_token": "rotated-refresh"
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
    let _env = EnvVarGuard::set(
        "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
        format!("http://{addr}/oauth/token"),
    );

    let home = tempfile::tempdir().unwrap();
    let credential_store_path = home.path().join("credentials.json");
    let valid_but_invalidated_material = json!({
        "tokens": {
            "access_token": make_token(json!({
                "exp": Utc::now().timestamp() + 3600,
                "chatgpt_account_id": "acct_profile"
            })),
            "refresh_token": "old-refresh",
            "account_id": "acct_profile"
        }
    })
    .to_string();
    let mut profiles = BTreeMap::new();
    profiles.insert(
        OPENAI_CODEX_CREDENTIAL_PROFILE.to_string(),
        CredentialProfileFile {
            kind: CredentialKind::OAuth,
            material: valid_but_invalidated_material.clone(),
        },
    );
    save_credential_store_at(&credential_store_path, &CredentialStoreFile { profiles }).unwrap();

    let mut provider_config = test_openai_codex_config(Some(valid_but_invalidated_material));
    provider_config.credential_store_path = Some(credential_store_path.clone());
    let provider = OpenAiCodexProvider::from_runtime_config(
        &provider_config,
        "gpt-codex-test",
        1024,
        home.path(),
        true,
    )
    .unwrap();

    let old_credential = provider.resolve_fresh_credential().await.unwrap();
    assert!(old_credential.source.starts_with("credential_profile:"));

    let refreshed = provider
        .refresh_after_auth_failure(&old_credential)
        .await
        .unwrap()
        .expect("profile auth failure should force refresh");

    assert_eq!(refreshed.account_id, "acct_profile");
    let store = load_credential_store_at(&credential_store_path).unwrap();
    let material = &store.profiles[OPENAI_CODEX_CREDENTIAL_PROFILE].material;
    assert!(material.contains("rotated-refresh"));
    assert!(!material.contains("old-refresh"));
    handle.abort();
}

#[tokio::test]
async fn openai_codex_refresh_fails_without_access_token() {
    let server = axum::Router::new().route(
        "/oauth/token",
        axum::routing::post(|| async {
            axum::Json(json!({
                "refresh_token": "rotated-refresh"
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
    let _env = EnvVarGuard::set(
        "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
        format!("http://{addr}/oauth/token"),
    );

    let home = tempfile::tempdir().unwrap();
    let credential_store_path = home.path().join("credentials.json");
    let expiring_material = json!({
        "tokens": {
            "access_token": make_token(json!({
                "exp": Utc::now().timestamp() + 30,
                "chatgpt_account_id": "acct_profile"
            })),
            "refresh_token": "old-refresh",
            "account_id": "acct_profile"
        }
    })
    .to_string();
    let mut profiles = BTreeMap::new();
    profiles.insert(
        OPENAI_CODEX_CREDENTIAL_PROFILE.to_string(),
        CredentialProfileFile {
            kind: CredentialKind::OAuth,
            material: expiring_material.clone(),
        },
    );
    save_credential_store_at(&credential_store_path, &CredentialStoreFile { profiles }).unwrap();

    let mut provider_config = test_openai_codex_config(Some(expiring_material));
    provider_config.credential_store_path = Some(credential_store_path.clone());
    let provider = OpenAiCodexProvider::from_runtime_config(
        &provider_config,
        "gpt-codex-test",
        1024,
        home.path(),
        true,
    )
    .unwrap();

    let error = provider
        .resolve_fresh_credential()
        .await
        .expect_err("refresh without an access token should fail");
    assert!(
        error
            .to_string()
            .contains("refresh response did not include an access token"),
        "{error}"
    );
    let store = load_credential_store_at(&credential_store_path).unwrap();
    let material = &store.profiles[OPENAI_CODEX_CREDENTIAL_PROFILE].material;
    assert!(material.contains("old-refresh"));
    assert!(!material.contains("rotated-refresh"));
    handle.abort();
}

#[test]
fn openai_codex_refresh_lock_uses_owner_only_permissions() {
    let home = tempfile::tempdir().unwrap();
    let lock_path = home.path().join("credentials.json.lock");
    let lock = CredentialStoreRefreshLock::acquire(&lock_path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    drop(lock);
    assert!(!lock_path.exists());
}

#[test]
fn openai_streaming_error_event_classifies_transient_server_codes_as_retryable() {
    for code in [
        "server_error",
        "service_unavailable",
        "server_is_overloaded",
        "slow_down",
    ] {
        let mut data_lines = vec![json!({
            "type": "error",
            "error": {
                "code": code,
                "message": "temporary server failure"
            }
        })
        .to_string()];

        let error = match consume_openai_sse_event(&mut data_lines) {
            Ok(_) => panic!("transient streaming error event should produce a provider error"),
            Err(error) => error,
        };
        let classification = classify_provider_error(&error);

        assert_eq!(classification.kind, ProviderFailureKind::ServerError);
        assert_eq!(classification.disposition, RetryDisposition::Retryable);
    }
}

#[test]
fn openai_streaming_error_event_keeps_unknown_codes_fail_fast_contract() {
    let mut data_lines = vec![json!({
        "type": "error",
        "error": {
            "code": "unexpected_protocol_state",
            "message": "unexpected stream shape"
        }
    })
    .to_string()];

    let error = match consume_openai_sse_event(&mut data_lines) {
        Ok(_) => panic!("unknown streaming error event should produce a provider error"),
        Err(error) => error,
    };
    let classification = classify_provider_error(&error);

    assert_eq!(classification.kind, ProviderFailureKind::ContractError);
    assert_eq!(classification.disposition, RetryDisposition::FailFast);
}

#[test]
fn openai_responses_request_lowers_native_web_search_tool() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("search the web".into())],
        vec![],
    );
    request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::OpenAi,
        provider_id: "openai_native".into(),
        provider_model_ref: "openai/gpt-test".into(),
        advertised_tool_type: "web_search_preview".into(),
        backend_kind: "openai_web_search".into(),
        max_results: Some(5),
    });

    let body = build_openai_responses_request(
        "gpt-test",
        1024,
        &request,
        OpenAiResponsesTransportContract::StandardJson,
        ToolSchemaContract::Strict,
        None,
        None,
    )
    .expect("openai responses request should build");

    assert!(body["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .any(|tool| tool == &json!({ "type": "web_search_preview" })));
}

#[test]
fn openai_responses_request_does_not_add_xai_x_search_tool() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("search X and the web".into())],
        vec![],
    );
    request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::Xai,
        provider_id: "xai".into(),
        provider_model_ref: "xai/grok-4-fast".into(),
        advertised_tool_type: "web_search".into(),
        backend_kind: "xai_web_search_x_search".into(),
        max_results: Some(5),
    });

    let body = build_openai_responses_request(
        "grok-4-fast",
        1024,
        &request,
        OpenAiResponsesTransportContract::StandardJson,
        ToolSchemaContract::Strict,
        Some("medium"),
        None,
    )
    .expect("xAI responses request should build");

    let tools = body["tools"].as_array().expect("tools should be an array");
    assert!(tools
        .iter()
        .any(|tool| tool == &json!({ "type": "web_search" })));
    assert!(!tools
        .iter()
        .any(|tool| tool == &json!({ "type": "x_search" })));
    assert_eq!(body["reasoning"]["effort"], json!("medium"));

    let diagnostics = native_web_search_diagnostics(&request)
        .expect("native web search diagnostics should be recorded");
    assert!(diagnostics.lowered);
    assert_eq!(diagnostics.kind, ProviderNativeWebSearchKind::Xai);
    assert_eq!(diagnostics.fallback_reason, None);
}

#[test]
fn openai_responses_request_lowers_json_schema_response_format() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("return json".into())],
        vec![],
    );
    request.response_format = Some(ProviderResponseFormatRequest::JsonSchema(
        ProviderJsonSchemaResponseFormat {
            name: "answer_v1".into(),
            strict: true,
            schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["answer"],
                "properties": {
                    "answer": { "type": "string" }
                }
            }),
        },
    ));

    let body = build_openai_responses_request(
        "gpt-test",
        1024,
        &request,
        OpenAiResponsesTransportContract::StandardJson,
        ToolSchemaContract::Strict,
        None,
        None,
    )
    .expect("openai responses request should build");

    assert!(body.get("response_format").is_none());
    assert_eq!(body["text"]["format"]["type"], json!("json_schema"));
    assert_eq!(body["text"]["format"]["name"], json!("answer_v1"));
    assert_eq!(body["text"]["format"]["strict"], json!(true));
    assert_eq!(
        body["text"]["format"]["schema"]["properties"]["answer"]["type"],
        json!("string")
    );
}

#[test]
fn openai_codex_responses_request_lowers_native_web_search_tool() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("search the web".into())],
        vec![],
    );
    request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::OpenAi,
        provider_id: "openai_codex_native".into(),
        provider_model_ref: "openai-codex/gpt-codex-test".into(),
        advertised_tool_type: "web_search".into(),
        backend_kind: "openai_codex_web_search".into(),
        max_results: Some(5),
    });

    let body = build_openai_responses_request(
        "gpt-codex-test",
        1024,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Relaxed,
        Some("low"),
        None,
    )
    .expect("openai codex responses request should build");

    assert!(body["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .any(|tool| tool == &json!({ "type": "web_search" })));
    assert_eq!(body["stream"], json!(true));
}

#[test]
fn openai_codex_image_generation_request_uses_hosted_tool() {
    let request = ProviderGenerateImageRequest {
        prompt: "draw a small holon".into(),
        size: Some("1024x1024".into()),
        background: Some("transparent".into()),
        output_format: Some("png".into()),
    };

    let body = build_openai_codex_image_generation_request("gpt-5.3-codex-spark", &request);

    assert_eq!(body["model"], json!("gpt-5.3-codex-spark"));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["store"], json!(false));
    assert_eq!(
        body["input"][0]["content"][0],
        json!({
            "type": "input_text",
            "text": "draw a small holon",
        })
    );
    assert_eq!(
        body["tools"][0],
        json!({
            "type": "image_generation",
            "output_format": "png",
            "size": "1024x1024",
            "background": "transparent",
        })
    );
}

#[test]
fn openai_codex_image_generation_response_parses_result_items() {
    let images = parse_openai_codex_image_generation_response_items(vec![
        json!({
            "type": "message",
            "content": [],
        }),
        json!({
            "type": "image_generation_call",
            "id": "ig_1",
            "status": "completed",
            "revised_prompt": "draw a tiny holon",
            "result": BASE64_STANDARD.encode(b"fake_png"),
        }),
    ])
    .expect("image_generation_call should parse");

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].bytes, b"fake_png");
    assert_eq!(images[0].mime.as_deref(), Some("image/png"));
}

#[test]
fn openai_codex_image_generation_response_accepts_done_item_with_generating_status() {
    let images = parse_openai_codex_image_generation_response_items(vec![json!({
        "type": "image_generation_call",
        "id": "ig_1",
        "status": "generating",
        "result": BASE64_STANDARD.encode(b"fake_png"),
    })])
    .expect("image_generation_call with a final result should parse");

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].bytes, b"fake_png");
}

#[test]
fn openai_responses_full_request_records_native_web_search_diagnostics() {
    let mut request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("search the web".into())],
        vec![],
    );
    request.native_web_search = Some(ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::OpenAi,
        provider_id: "openai_codex_native".into(),
        provider_model_ref: "openai-codex/gpt-codex-test".into(),
        advertised_tool_type: "web_search".into(),
        backend_kind: "openai_codex_web_search".into(),
        max_results: Some(5),
    });

    let body = build_openai_responses_request(
        "gpt-codex-test",
        1024,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Relaxed,
        Some("low"),
        None,
    )
    .expect("openai codex responses request should build");
    let plan = plan_openai_responses_request(
        body,
        &request,
        &Arc::new(Mutex::new(OpenAiContinuationState::default())),
        false,
        OpenAiResponsesContinuationContract::Standard,
    )
    .expect("openai codex responses request should plan");

    let diagnostics = plan
        .diagnostics
        .native_web_search
        .expect("native web search diagnostics should be recorded");
    assert!(diagnostics.lowered);
    assert_eq!(diagnostics.advertised_tool_type, "web_search");
    assert_eq!(diagnostics.backend_kind, "openai_codex_web_search");
}

#[test]
fn openai_codex_request_omits_reasoning_when_supports_reasoning_is_false() {
    // Negative-path guard test: when supports_reasoning=false, the provider
    // passes reasoning_effort=None to build_openai_responses_request, which
    // must produce a body with reasoning=null and no reasoning.effort field.
    let request = ProviderTurnRequest::plain(
        "system",
        vec![ConversationMessage::UserText("hello".into())],
        vec![],
    );

    // Guard suppressed path (supports_reasoning=false → None)
    let suppressed = build_openai_responses_request(
        "gpt-test",
        4096,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Relaxed,
        None,
        None,
    )
    .expect("suppressed reasoning request should build");
    assert_eq!(suppressed["reasoning"], json!(null));
    assert!(suppressed["reasoning"].get("effort").is_none());

    // Guard enabled path (supports_reasoning=true → Some)
    let enabled = build_openai_responses_request(
        "gpt-test",
        4096,
        &request,
        OpenAiResponsesTransportContract::CodexStreaming,
        ToolSchemaContract::Relaxed,
        Some("low"),
        None,
    )
    .expect("enabled reasoning request should build");
    assert_eq!(enabled["reasoning"]["effort"], json!("low"));
}

#[test]
fn openai_provider_window_tracks_latest_compaction_item() {
    let items = vec![
        json!({ "type": "message", "role": "user" }),
        json!({ "type": "compaction", "encrypted_content": "first" }),
        json!({ "type": "message", "role": "user" }),
        json!({ "type": "compaction", "encrypted_content": "second" }),
    ];

    assert_eq!(latest_openai_compaction_index(&items), Some(3));
}

#[test]
fn openai_compaction_trigger_skips_many_small_items_below_budget() {
    let request_shape = test_request_shape();
    let window = OpenAiProviderWindow {
        response_id: Some("resp_1".into()),
        request_shape: request_shape.clone(),
        items: (0..24)
            .map(|index| json!({ "type": "message", "content": format!("m{index}") }))
            .collect(),
        append_match_items: Vec::new(),
        latest_compaction_index: None,
        latest_input_tokens: 0,
        replay_loss_reason: None,
        generation: 1,
    };

    assert!(openai_compaction_trigger_for_window(
        &window,
        &request_shape,
        OpenAiCompactionPolicy {
            trigger_input_tokens: 10_000,
        },
    )
    .is_none());
}

#[test]
fn openai_compaction_candidate_allows_single_large_item() {
    let request_shape = test_request_shape();
    let window = OpenAiProviderWindow {
        response_id: Some("resp_1".into()),
        request_shape: request_shape.clone(),
        items: vec![json!({
            "type": "message",
            "content": "x".repeat(4096),
        })],
        append_match_items: Vec::new(),
        latest_compaction_index: None,
        latest_input_tokens: 0,
        replay_loss_reason: None,
        generation: 1,
    };

    let trigger = openai_compaction_trigger_for_window(
        &window,
        &request_shape,
        OpenAiCompactionPolicy {
            trigger_input_tokens: 128,
        },
    )
    .expect("large item should reach token pressure");
    assert_eq!(trigger.reason, "estimated_window_pressure");

    let candidate = openai_provider_window_compaction_candidate(&window)
        .expect("single complete message should be compactable");
    assert_eq!(candidate.items.len(), 1);
}

#[test]
fn openai_compaction_trigger_prefers_provider_usage_tokens() {
    let request_shape = test_request_shape();
    let window = OpenAiProviderWindow {
        response_id: Some("resp_1".into()),
        request_shape: request_shape.clone(),
        items: vec![json!({ "type": "message", "content": "small" })],
        append_match_items: Vec::new(),
        latest_compaction_index: None,
        latest_input_tokens: 512,
        replay_loss_reason: None,
        generation: 1,
    };

    let trigger = openai_compaction_trigger_for_window(
        &window,
        &request_shape,
        OpenAiCompactionPolicy {
            trigger_input_tokens: 128,
        },
    )
    .expect("usage should reach token pressure");
    assert_eq!(trigger.reason, "token_budget_pressure");
    assert_eq!(trigger.estimated_input_tokens, None);
}

#[test]
fn openai_compaction_trigger_skips_immediate_compacted_replay_before_usage() {
    let request_shape = test_request_shape();
    let previous = OpenAiProviderWindow {
        response_id: Some("resp_1".into()),
        request_shape: request_shape.clone(),
        items: vec![
            json!({ "type": "compaction", "encrypted_content": "opaque" }),
            json!({ "type": "message", "content": "recent" }),
        ],
        append_match_items: Vec::new(),
        latest_compaction_index: Some(0),
        latest_input_tokens: 0,
        replay_loss_reason: None,
        generation: 1,
    };
    let plan = OpenAiRequestPlan {
        body: json!({ "model": "gpt-test", "input": [] }),
        fallback_replay: None,
        scope: None,
        append_match_input: Vec::new(),
        provider_input: vec![json!({ "type": "message", "content": "continue" })],
        replay_loss_reason: None,
        request_shape,
        diagnostics: incremental_diagnostics(
            "provider_window_compacted",
            "test",
            None,
            0,
            None,
            None,
            None,
            None,
        ),
    };

    assert!(openai_compaction_trigger_for_request_plan(
        &previous,
        &plan,
        OpenAiCompactionPolicy {
            trigger_input_tokens: 1,
        },
    )
    .is_none());
}

fn test_request_shape() -> OpenAiRequestShape {
    OpenAiRequestShape {
        wire_shape: json!({ "model": "gpt-test" }),
        prompt_frame: crate::provider::ProviderPromptFrame::plain("system"),
    }
}
