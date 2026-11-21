use super::adapters;
use crate::config::{
    load_credential_store_at, save_credential_store_at, CredentialKind, CredentialProfileFile,
    CredentialSource, CredentialStoreFile, ProviderAuthConfig, ProviderEndpointId, ProviderId,
    ProviderRuntimeConfig, ProviderTransportKind, OPENAI_CODEX_CREDENTIAL_PROFILE,
};
use crate::provider::retry::{classify_provider_error, ProviderFailureKind, RetryDisposition};
use crate::provider::{OpenAiCodexProvider, ProviderGenerateImageRequest};
use base64::Engine;
use chrono::Utc;
use serde_json::json;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::PathBuf,
    sync::{Arc, Mutex},
};

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
        headers: Default::default(),
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
            client_id: None,
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

#[tokio::test]
async fn oauth_refresh_rejection_is_classified_and_safely_explained() {
    use crate::{
        config::StateMemoryLimits,
        diagnostics::resource_io::ResourceReadMaterial,
        provider::{AgentProvider, ProviderRequestMaterial},
        runtime::{
            input_evidence_budget::InputEvidenceBudgetLedger,
            resident_input_evidence::InputEvidenceScope,
        },
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    let _env_lock = CODEX_REFRESH_ENV_LOCK.lock().unwrap();
    for (status, body, expected_kind, reason) in [
        (
            reqwest::StatusCode::BAD_REQUEST,
            json!({"error": "invalid_grant", "error_description": "invalid token: private-upstream-value"}),
            ProviderFailureKind::AuthError,
            "rejected",
        ),
        (
            reqwest::StatusCode::UNAUTHORIZED,
            json!({"error": {"code": "refresh_token_expired"}}),
            ProviderFailureKind::AuthError,
            "expired",
        ),
        (
            reqwest::StatusCode::FORBIDDEN,
            json!({"error": {"code": "refresh_token_revoked"}}),
            ProviderFailureKind::AuthError,
            "revoked",
        ),
        (
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "invalid_grant", "error_description": "private-upstream-value"}),
            ProviderFailureKind::Unknown,
            "HTTP 429",
        ),
        (
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": {"code": "refresh_token_expired", "message": "private-upstream-value"}}),
            ProviderFailureKind::Unknown,
            "HTTP 503",
        ),
        (
            reqwest::StatusCode::OK,
            json!({"refresh_token": "private-upstream-value"}),
            ProviderFailureKind::Unknown,
            "response",
        ),
    ] {
        let image_calls = Arc::new(AtomicUsize::new(0));
        let calls = image_calls.clone();
        let server = axum::Router::new()
            .route(
                "/oauth/token",
                axum::routing::post(move || async move { (status, axum::Json(body)) }),
            )
            .route(
                "/codex/responses",
                axum::routing::post(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async {
                        (
                            reqwest::StatusCode::UNAUTHORIZED,
                            axum::Json(json!({"error": "unauthorized"})),
                        )
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        });
        let _env = EnvVarGuard::set(
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
            format!("http://{addr}/oauth/token"),
        );
        let home = tempfile::tempdir().unwrap();
        let store_path = home.path().join("credentials.json");
        let material = json!({
            "tokens": {
                "access_token": make_token(json!({
                    "exp": Utc::now().timestamp() + 30,
                    "chatgpt_account_id": "acct_profile"
                })),
                "refresh_token": "private-refresh-value",
                "account_id": "acct_profile"
            }
        })
        .to_string();
        save_credential_store_at(
            &store_path,
            &CredentialStoreFile {
                profiles: BTreeMap::from([(
                    OPENAI_CODEX_CREDENTIAL_PROFILE.to_string(),
                    CredentialProfileFile {
                        kind: CredentialKind::OAuth,
                        material: material.clone(),
                    },
                )]),
            },
        )
        .unwrap();
        let mut config = test_openai_codex_config(Some(material.clone()));
        config.credential_store_path = Some(store_path.clone());
        let provider = OpenAiCodexProvider::from_runtime_config(
            &config,
            "gpt-codex-test",
            1024,
            home.path(),
            true,
        )
        .unwrap();

        let error = provider.0.resolve_fresh_credential().await.unwrap_err();
        // Credential resolution (also used after an API auth rejection) must
        // preserve the typed refresh failure instead of wrapping it as auth.
        let error = adapters::openai::openai_codex_credential_resolution_error(
            error,
            "openai-codex@default/gpt-codex-test",
            false,
        );
        let classification = classify_provider_error(&error);
        assert_eq!(classification.kind, expected_kind, "{error:#}");
        assert_eq!(classification.disposition, RetryDisposition::FailFast);
        assert_eq!(
            crate::provider::retry::provider_fallback_disposition(classification.kind),
            crate::provider::ProviderFallbackDisposition::Immediate
        );
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert!(!descriptor.retryable);
        assert!(descriptor.operator_message.contains("openai-codex"));
        assert!(
            descriptor.operator_message.contains(reason),
            "{descriptor:?}"
        );
        assert!(!descriptor
            .operator_message
            .contains("private-upstream-value"));
        assert!(!descriptor
            .operator_message
            .contains("private-refresh-value"));
        assert_eq!(
            descriptor.recovery_hint.is_some(),
            expected_kind == ProviderFailureKind::AuthError
        );
        assert_eq!(
            load_credential_store_at(&store_path).unwrap().profiles
                [OPENAI_CODEX_CREDENTIAL_PROFILE]
                .material,
            material
        );
        if status == reqwest::StatusCode::BAD_REQUEST {
            for expires_in in [30, 3600] {
                let image_credential = json!({
                    "tokens": {
                        "access_token": make_token(json!({
                            "exp": Utc::now().timestamp() + expires_in,
                            "chatgpt_account_id": "acct_profile"
                        })),
                        "refresh_token": "private-refresh-value",
                        "account_id": "acct_profile"
                    }
                })
                .to_string();
                let mut store = load_credential_store_at(&store_path).unwrap();
                store
                    .profiles
                    .get_mut(OPENAI_CODEX_CREDENTIAL_PROFILE)
                    .unwrap()
                    .material = image_credential.clone();
                save_credential_store_at(&store_path, &store).unwrap();
                let mut image_config = test_openai_codex_config(Some(image_credential));
                image_config.credential_store_path = Some(store_path.clone());
                image_config.base_url = format!("http://{addr}/codex");
                let image_provider = OpenAiCodexProvider::from_runtime_config(
                    &image_config,
                    "gpt-codex-test",
                    1024,
                    home.path(),
                    true,
                )
                .unwrap();
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default()).unwrap();
                let scope = InputEvidenceScope {
                    agent_id: "refresh-image-test".into(),
                    identity_incarnation: 1,
                };
                let image_error = image_provider
                    .generate_image(ProviderGenerateImageRequest {
                        material: ProviderRequestMaterial::scoped(
                            ResourceReadMaterial::new(&ledger, &scope).unwrap(),
                        ),
                        prompt: "image".into(),
                        size: None,
                        aspect_ratio: None,
                        resolution: None,
                        background: None,
                        output_format: None,
                    })
                    .await
                    .unwrap_err();
                let diagnostics =
                    crate::provider::provider_transport_diagnostics(&image_error).unwrap();
                assert_eq!(diagnostics.stage, "oauth_refresh");
                assert_eq!(diagnostics.status, Some(400));
                assert_eq!(
                    classify_provider_error(&image_error).kind,
                    ProviderFailureKind::AuthError
                );
                let descriptor = crate::runtime_error::describe_runtime_error(&image_error);
                assert!(descriptor.operator_message.contains("refresh_failed"));
                assert!(!descriptor
                    .operator_message
                    .contains("private-upstream-value"));
                assert!(!descriptor
                    .operator_message
                    .contains("private-refresh-value"));
                assert!(descriptor.recovery_hint.unwrap().contains("sign in again"));
                // Expiring tokens fail before the image API; valid tokens reach
                // its 401 response before forced refresh fails.
                assert_eq!(
                    image_calls.load(Ordering::SeqCst),
                    usize::from(expires_in == 3600)
                );
            }
        }
        handle.abort();
    }
}
