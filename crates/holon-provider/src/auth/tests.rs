use super::*;
use base64::Engine;
use serde_json::json;

/// Restores an environment variable when the guard is dropped.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
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

/// Serializes the tests that override process-wide environment variables.
#[test]
fn oauth_refresh_http_classification_requires_rejected_grant_status() {
    for status in [400, 401, 403, 429, 500, 503] {
        for (code, rejected_kind) in [
            (
                "refresh_token_expired",
                CodexOAuthRefreshFailureKind::Expired,
            ),
            ("refresh_token_reused", CodexOAuthRefreshFailureKind::Reused),
            (
                "refresh_token_revoked",
                CodexOAuthRefreshFailureKind::Revoked,
            ),
            ("invalid_grant", CodexOAuthRefreshFailureKind::Rejected),
        ] {
            let body = json!({"error": {"code": code}}).to_string();
            let expected = if matches!(status, 400 | 401 | 403) {
                rejected_kind
            } else {
                CodexOAuthRefreshFailureKind::Other
            };
            assert_eq!(
                classify_refresh_failure_kind(
                    reqwest::StatusCode::from_u16(status).unwrap(),
                    &body
                ),
                expected
            );
        }
    }
}

#[tokio::test]
async fn oauth_refresh_discovery_failure_is_not_rejected_credentials() {
    let mut config = OAuthProviderConfig::xai();
    // Rejected before any network request, at the real discovery boundary.
    config.issuer = "http://invalid.example";
    let failure = refresh_oauth_profile_material(
        &reqwest::Client::new(),
        &config,
        r#"{"tokens":{"access_token":"access","refresh_token":"private-refresh-value"}}"#,
        "test-profile",
    )
    .await
    .unwrap_err();
    assert_eq!(failure.kind, CodexOAuthRefreshFailureKind::Discovery);
    assert!(!failure.credentials_rejected());
    assert!(failure.safe_summary().contains("endpoint discovery"));
    assert!(!failure.safe_summary().contains("private-refresh-value"));
}

#[tokio::test]
async fn github_refresh_parse_errors_do_not_include_credential_values() {
    let client = reqwest::Client::new();
    let config = OAuthProviderConfig::github_copilot("Iv1.config-client");
    for material in [
        r#"{"tokens":"refresh-secret"}"#,
        r#"{"tokens":"{\"access_token\":\"access-secret\",\"refresh_token\":\"refresh-secret\",\"id_token\":\"id-secret\"}"}"#,
    ] {
        let failure = refresh_oauth_profile_material(&client, &config, material, "github-copilot")
            .await
            .unwrap_err();
        assert_eq!(failure.kind, CodexOAuthRefreshFailureKind::Other);
        let rendered = format!("{failure:#?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
        assert!(!rendered.contains("id-secret"));
        assert!(!failure.safe_summary().contains("refresh-secret"));
    }
}

#[tokio::test]
async fn github_refresh_uses_the_client_id_recorded_in_the_material() {
    let captured_forms: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = captured_forms.clone();
    let server = axum::Router::new().route(
        "/oauth/token",
        axum::routing::post(move |body: String| {
            let captured = captured.clone();
            async move {
                captured.lock().unwrap().push(body);
                axum::Json(json!({
                    "access_token": "new-access",
                    "refresh_token": "rotated-refresh",
                    "expires_in": 28800
                }))
            }
        }),
    );
    let _env_lock = GITHUB_TOKEN_URL_TEST_ENV_LOCK.lock().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let _env = EnvVarGuard::set(
        GITHUB_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("http://{addr}/oauth/token"),
    );

    let material = json!({
        "tokens": {
            "access_token": "old-access",
            "refresh_token": "old-refresh"
        },
        "oauth_client_id": "Iv1.user-owned-client"
    })
    .to_string();
    let refreshed = refresh_oauth_profile_material(
        &reqwest::Client::new(),
        // A different configured client id must not win over the
        // client id recorded in the material.
        &OAuthProviderConfig::github_copilot("Iv1.config-client"),
        &material,
        "github-copilot",
    )
    .await
    .expect("GitHub OAuth refresh should succeed");

    assert_eq!(refreshed.credential.access_token, "new-access");
    let auth: AuthDotJson =
        serde_json::from_str(&refreshed.material).expect("refreshed material should parse");
    assert_eq!(
        auth.tokens.expect("tokens").refresh_token,
        "rotated-refresh"
    );
    assert_eq!(
        auth.oauth_client_id.as_deref(),
        Some("Iv1.user-owned-client")
    );
    let forms = captured_forms.lock().unwrap();
    assert_eq!(forms.len(), 1);
    assert!(forms[0].contains("client_id=Iv1.user-owned-client"));
    assert!(!forms[0].contains("Iv1.config-client"));
    assert!(forms[0].contains("grant_type=refresh_token"));
    handle.abort();
}

#[tokio::test]
async fn github_refresh_without_a_refresh_token_reports_retirement() {
    let material = json!({
        "tokens": {
            "access_token": "old-access"
        },
        "oauth_client_id": "Iv1.user-owned-client"
    })
    .to_string();
    let failure = refresh_oauth_profile_material(
        &reqwest::Client::new(),
        &OAuthProviderConfig::github_copilot("Iv1.user-owned-client"),
        &material,
        "github-copilot",
    )
    .await
    .unwrap_err();

    assert_eq!(failure.kind, CodexOAuthRefreshFailureKind::Rejected);
    assert!(failure
        .message
        .contains("provider github-copilot is retired; choose a supported provider"));
    assert!(!failure.message.contains("old-access"));
}

#[test]
fn oauth_refresh_non_grant_failures_have_safe_phase_reasons() {
    for (kind, reason) in [
        (
            CodexOAuthRefreshFailureKind::Discovery,
            "endpoint discovery",
        ),
        (CodexOAuthRefreshFailureKind::Transport, "transport"),
        (CodexOAuthRefreshFailureKind::InvalidResponse, "response"),
        (CodexOAuthRefreshFailureKind::Other, "refresh failed"),
    ] {
        let failure = CodexOAuthRefreshFailure {
            kind,
            status: None,
            message: "private-upstream-value".into(),
        };
        assert!(!failure.credentials_rejected());
        let summary = failure.safe_summary();
        assert!(summary.contains(reason));
        assert!(summary.contains("refresh_failed"));
        assert!(!summary.contains("private-upstream-value"));
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

#[test]
fn extracts_account_id_from_nested_access_claims() {
    let token = make_token(json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_nested"
        }
    }));

    assert_eq!(
        extract_account_id_from_access_token(&token).as_deref(),
        Some("acct_nested")
    );
}

#[test]
fn extracts_account_id_from_chatgpt_account_id_claim() {
    let token = make_token(json!({
        "chatgpt_account_id": "acct_chatgpt"
    }));

    assert_eq!(
        extract_account_id_from_access_token(&token).as_deref(),
        Some("acct_chatgpt")
    );
}

#[test]
fn auth_to_credential_uses_id_token_object_fallback() {
    let credential = auth_to_credential(
        AuthDotJson {
            tokens: Some(TokenData {
                access_token: make_token(json!({
                    "exp": 1_900_000_000
                })),
                refresh_token: "refresh".to_string(),
                account_id: None,
                id_token: Some(json!({
                    "chatgpt_account_id": "acct_id_token"
                })),
            }),
            last_refresh: None,
            expires_at: None,
            oauth_client_id: None,
        },
        "file",
    )
    .expect("id_token object fallback should resolve account id");

    assert_eq!(credential.account_id, "acct_id_token");
    assert_eq!(credential.source, "file");
}

#[test]
fn codex_oauth_login_result_material_is_profile_compatible() {
    let result = codex_oauth_tokens_to_login_result(LoginTokenResponse {
        id_token: make_token(json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_login"
            }
        })),
        access_token: make_token(json!({
            "exp": 1_900_000_000
        })),
        refresh_token: "refresh_login".to_string(),
    })
    .expect("login material should serialize");

    let credential = load_codex_oauth_profile_credential(&result.material, "openai-codex")
        .expect("login material should parse as a Holon OAuth profile");

    assert_eq!(result.account_id.as_deref(), Some("acct_login"));
    assert_eq!(credential.account_id, "acct_login");
    assert_eq!(credential.source, "credential_profile:openai-codex");
}

#[test]
fn xai_oauth_config_matches_public_cli_contract() {
    let config = OAuthProviderConfig::xai();

    assert_eq!(config.issuer, "https://auth.x.ai");
    assert_eq!(config.client_id, "b1a00492-073a-47ea-816f-4c329264a828");
    assert!(config.scopes.contains(&"grok-cli:access"));
    assert!(config.scopes.contains(&"api:access"));
    assert_eq!(config.device_protocol, OAuthDeviceProtocol::Standard);
}

#[test]
fn xai_oauth_endpoint_validation_accepts_xai_hosts_only() {
    let config = OAuthProviderConfig::xai();

    validate_oauth_endpoint(&config, "https://auth.x.ai/oauth2/token")
        .expect("issuer host should be accepted");
    validate_oauth_endpoint(&config, "https://login.auth.x.ai/oauth2/token")
        .expect("issuer subdomain should be accepted");
    assert!(validate_oauth_endpoint(&config, "http://auth.x.ai/oauth2/token").is_err());
    assert!(validate_oauth_endpoint(&config, "https://x.ai.evil.example/token").is_err());
    assert!(validate_oauth_endpoint(&config, "https://example.com/token").is_err());
}

#[test]
fn xai_oauth_material_uses_existing_profile_shape() {
    let result = oauth_tokens_to_login_result(
        &OAuthProviderConfig::xai(),
        make_token(json!({"exp": 1_900_000_000, "sub": "user_123"})),
        "refresh_xai".to_string(),
        None,
    )
    .expect("xAI OAuth material should serialize");
    let auth: AuthDotJson =
        serde_json::from_str(&result.material).expect("material should use auth.json shape");

    let tokens = auth.tokens.expect("tokens should be present");
    assert_eq!(tokens.refresh_token, "refresh_xai");
    assert_eq!(tokens.account_id, None);
    assert_eq!(result.account_id, None);
}

#[test]
fn standard_oauth_refresh_rotates_refresh_token() {
    let mut auth = AuthDotJson {
        tokens: Some(TokenData {
            access_token: "old-access".into(),
            refresh_token: "old-refresh".into(),
            account_id: None,
            id_token: None,
        }),
        last_refresh: None,
        expires_at: None,
        oauth_client_id: None,
    };

    apply_standard_token_response(
        &mut auth,
        StandardTokenResponse {
            access_token: "new-access".into(),
            refresh_token: "rotated-refresh".into(),
            id_token: Some("new-id-token".into()),
        },
    );

    let tokens = auth.tokens.expect("tokens should remain present");
    assert_eq!(tokens.access_token, "new-access");
    assert_eq!(tokens.refresh_token, "rotated-refresh");
    assert_eq!(tokens.id_token, Some(Value::String("new-id-token".into())));
    assert!(auth.last_refresh.is_some());
}

#[test]
fn device_user_code_response_accepts_string_and_numeric_timing_fields() {
    let response: DeviceUserCodeResponse = serde_json::from_value(json!({
        "device_auth_id": "auth_123",
        "usercode": "ABCD-EFGH",
        "interval": "2",
        "expires_in": "600"
    }))
    .expect("string timing fields should parse");

    assert_eq!(response.device_auth_id, "auth_123");
    assert_eq!(response.user_code, "ABCD-EFGH");
    assert_eq!(response.interval, 2);
    assert_eq!(response.expires_in, Some(600));

    let response: DeviceUserCodeResponse = serde_json::from_value(json!({
        "device_auth_id": "auth_456",
        "user_code": "WXYZ-1234",
        "interval": 5,
        "expires_in": 300
    }))
    .expect("numeric timing fields should parse");

    assert_eq!(response.interval, 5);
    assert_eq!(response.expires_in, Some(300));
}

#[test]
fn classify_device_poll_status_uses_oauth_error_codes() {
    assert_eq!(
        classify_device_poll_status(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"authorization_pending"}"#
        ),
        DevicePollStatus::Pending
    );
    assert_eq!(
        classify_device_poll_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":"slow_down"}"#
        ),
        DevicePollStatus::SlowDown
    );
    assert_eq!(
        classify_device_poll_status(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"expired_token"}"#
        ),
        DevicePollStatus::Expired
    );
    assert_eq!(
        classify_device_poll_status(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"access_denied"}"#
        ),
        DevicePollStatus::AccessDenied
    );
    assert_eq!(
        classify_device_poll_status(reqwest::StatusCode::FORBIDDEN, ""),
        DevicePollStatus::Pending
    );
    assert_eq!(
        classify_device_poll_status(reqwest::StatusCode::TOO_MANY_REQUESTS, ""),
        DevicePollStatus::SlowDown
    );
    // OpenAI returns HTTP 403 with a non-standard error code when the
    // device authorization is still pending. This should be treated as
    // Pending, not Failed.
    assert_eq!(
        classify_device_poll_status(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"code":"forbidden","message":"Device authorization is pending. Please try again."}"#
        ),
        DevicePollStatus::Pending
    );
    // But access_denied on 403 should still be terminal.
    assert_eq!(
        classify_device_poll_status(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"error":"access_denied"}"#
        ),
        DevicePollStatus::AccessDenied
    );
}

#[test]
fn keychain_account_matches_cli_prefix_shape() {
    let account = compute_codex_keychain_account(std::path::Path::new("/tmp/codex-home"));
    assert!(account.starts_with("cli|"));
    assert_eq!(account.len(), 20);
}

#[test]
fn retired_github_copilot_is_not_available_for_device_login() {
    assert!(oauth_provider_config(RETIRED_GITHUB_COPILOT_PROVIDER).is_none());
    assert!(!provider_supports_oauth_device_login(
        RETIRED_GITHUB_COPILOT_PROVIDER
    ));
    for provider in [RETIRED_GITHUB_COPILOT_PROVIDER, " GitHub-Copilot "] {
        for hint in [None, Some("old-app-client")] {
            let error = resolve_oauth_provider_config(provider, hint).unwrap_err();
            assert_eq!(
                error.to_string(),
                "provider github-copilot is retired; choose a supported provider"
            );
        }
    }
}

#[test]
fn supported_oauth_device_login_providers_are_unchanged() {
    for (provider, expected) in [
        ("openai-codex", "openai-codex"),
        ("codex", "openai-codex"),
        ("xai", "xai"),
    ] {
        assert!(provider_supports_oauth_device_login(provider));
        let config = resolve_oauth_provider_config(provider, None)
            .unwrap()
            .unwrap();
        assert_eq!(config.provider_id, expected);
    }
    assert!(resolve_oauth_provider_config("anthropic", None)
        .unwrap()
        .is_none());
}

#[test]
fn stored_github_copilot_profiles_remain_readable() {
    let stored = r#"{
        "oauth_client_id":"old-app-client",
        "tokens":{"access_token":"old-access-token","refresh_token":""},
        "last_refresh":"2025-01-01T00:00:00Z"
    }"#;
    let profile = load_oauth_profile_credential(stored, RETIRED_GITHUB_COPILOT_PROVIDER).unwrap();
    assert_eq!(profile.access_token, "old-access-token");
    assert!(profile.expires_at.is_none());
}

#[tokio::test]
async fn legacy_refresh_config_cannot_start_or_complete_device_login() {
    let config = OAuthProviderConfig::github_copilot("old-app-client");
    let error = request_oauth_device_code(&config).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "provider github-copilot is retired; choose a supported provider"
    );
    let code = CodexDeviceCode {
        provider_id: config.provider_id.to_string(),
        verification_url: String::new(),
        user_code: String::new(),
        interval: 0,
        expires_at: Utc::now(),
        secret: String::new(),
        token_endpoint: None,
    };
    let error = complete_oauth_device_login(&config, code)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "provider github-copilot is retired; choose a supported provider"
    );
}

#[test]
fn oauth_device_poll_accepts_standard_error_codes_with_http_success() {
    assert_eq!(
        classify_device_poll_status(
            reqwest::StatusCode::OK,
            r#"{"error":"authorization_pending"}"#
        ),
        DevicePollStatus::Pending
    );
    assert_eq!(
        classify_device_poll_status(reqwest::StatusCode::OK, r#"{"error":"slow_down"}"#),
        DevicePollStatus::SlowDown
    );
    assert_eq!(
        classify_device_poll_status(reqwest::StatusCode::OK, r#"{"error":"expired_token"}"#),
        DevicePollStatus::Expired
    );
    assert_eq!(
        classify_device_poll_status(reqwest::StatusCode::OK, r#"{"error":"access_denied"}"#),
        DevicePollStatus::AccessDenied
    );
}
