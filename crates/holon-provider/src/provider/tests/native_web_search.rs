//! Standalone native web search contract tests for the OpenAI Responses and
//! Codex transports: request boundary, structured source mapping, and
//! WebSearch error-kind classification. No real credentials are used.

use std::sync::{Arc, Mutex};

use super::support::{codex_provider, spawn_test_server, ProviderFixture, TEST_CLIENT_VERSION};
use super::*;
use crate::provider::retry::{
    provider_transport_error, ProviderFailureClassification, ProviderFailureKind, RetryDisposition,
};
use crate::provider::transports::openai::native_search_transport_failure;
use crate::provider::{AgentProvider, ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

fn openai_native_search_request(
    provider_id: &str,
    model_ref: &str,
    advertised_tool_type: &str,
    backend_kind: &str,
) -> ProviderNativeWebSearchRequest {
    ProviderNativeWebSearchRequest {
        kind: ProviderNativeWebSearchKind::OpenAi,
        provider_id: provider_id.into(),
        provider_model_ref: model_ref.into(),
        advertised_tool_type: advertised_tool_type.into(),
        backend_kind: backend_kind.into(),
        max_results: Some(5),
    }
}

fn native_search_response_with_citations(response_id: &str) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 12, "output_tokens": 2 },
        "output": [
            { "type": "web_search_call", "id": "ws_1", "status": "completed" },
            {
                "type": "message",
                "id": "msg_1",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "summary",
                    "annotations": [
                        {
                            "type": "url_citation",
                            "url": "https://example.com/first",
                            "title": "First Source"
                        },
                        {
                            "type": "url_citation",
                            "url": "https://example.com/first",
                            "title": "Duplicate URL Within Item"
                        },
                        {
                            "type": "url_citation",
                            "url": "https://example.com/second"
                        },
                        {
                            "type": "file_citation",
                            "file_id": "file_ignored"
                        }
                    ]
                }]
            }
        ]
    })
}

fn native_search_sse_response(response_id: &str) -> String {
    format!(
        "event: response.completed\ndata: {}\n\n",
        json!({
            "type": "response.completed",
            "response": native_search_response_with_citations(response_id),
        })
    )
}

#[tokio::test]
async fn openai_native_web_search_sends_minimal_request_and_maps_citations() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                Json(native_search_response_with_citations("resp_native_1"))
            }
        }),
    ))
    .await;
    let fixture = ProviderFixture::openai("native-search-key");
    let mut config = fixture.config.clone();
    config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &config,
        "gpt-search",
        1024,
        fixture.home.path(),
        TEST_CLIENT_VERSION,
    )
    .unwrap();

    let outcome = provider
        .native_web_search(
            openai_native_search_request(
                "openai-search",
                "openai-search/gpt-search",
                "web_search_preview",
                "openai_web_search",
            ),
            "rust async runtime",
        )
        .await
        .expect("native web search should succeed");

    assert!(outcome.search_executed);
    let urls: Vec<&str> = outcome
        .citations
        .iter()
        .map(|citation| citation.url.as_str())
        .collect();
    assert_eq!(
        urls,
        ["https://example.com/first", "https://example.com/second"]
    );
    assert_eq!(outcome.citations[0].title.as_deref(), Some("First Source"));
    // The second citation carries no title; optional fields stay absent.
    assert_eq!(outcome.citations[1].title, None);

    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    // Independent request boundary: only the query, no conversation history.
    assert_eq!(body["instructions"], json!("rust async runtime"));
    let input = body["input"].as_array().expect("input should be an array");
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], json!("user"));
    assert_eq!(input[0]["content"][0]["text"], json!("rust async runtime"));
    // Only the native web search tool is attached; no other tools.
    assert_eq!(body["tools"], json!([{ "type": "web_search_preview" }]));
    assert_eq!(body["tool_choice"], json!("auto"));
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["max_output_tokens"], json!(256));
    // No response format or extra prompt controls on the search boundary.
    assert!(body.get("text").is_none());
    assert!(body.get("response_format").is_none());
    assert!(body.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn openai_native_web_search_returns_empty_results_when_search_ran_without_sources() {
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || async {
            Json(json!({
                "id": "resp_empty_search",
                "status": "completed",
                "usage": { "input_tokens": 4, "output_tokens": 1 },
                "output": [
                    { "type": "web_search_call", "id": "ws_1", "status": "completed" },
                    {
                        "type": "message",
                        "id": "msg_1",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "no sources" }]
                    }
                ]
            }))
        }),
    ))
    .await;
    let fixture = ProviderFixture::openai("native-search-key");
    let mut config = fixture.config.clone();
    config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &config,
        "gpt-search",
        1024,
        fixture.home.path(),
        TEST_CLIENT_VERSION,
    )
    .unwrap();

    let outcome = provider
        .native_web_search(
            openai_native_search_request(
                "openai-search",
                "openai-search/gpt-search",
                "web_search_preview",
                "openai_web_search",
            ),
            "obscure query",
        )
        .await
        .expect("executed search without sources is a successful empty result");

    assert!(outcome.search_executed);
    assert!(outcome.citations.is_empty());
}

#[tokio::test]
async fn openai_native_web_search_marks_pure_answer_as_search_not_executed() {
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || async {
            Json(json!({
                "id": "resp_pure_answer",
                "status": "completed",
                "usage": { "input_tokens": 4, "output_tokens": 1 },
                "output": [{
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "pure answer"
                    }]
                }]
            }))
        }),
    ))
    .await;
    let fixture = ProviderFixture::openai("native-search-key");
    let mut config = fixture.config.clone();
    config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &config,
        "gpt-search",
        1024,
        fixture.home.path(),
        TEST_CLIENT_VERSION,
    )
    .unwrap();

    let outcome = provider
        .native_web_search(
            openai_native_search_request(
                "openai-search",
                "openai-search/gpt-search",
                "web_search_preview",
                "openai_web_search",
            ),
            "anything",
        )
        .await
        .expect("transport-level outcome is still Ok; the host maps the flag");

    assert!(!outcome.search_executed);
    assert!(outcome.citations.is_empty());
}

#[tokio::test]
async fn openai_native_web_search_classifies_auth_and_rate_limit_without_leaking_credentials() {
    for (status, expected_kind) in [
        (StatusCode::UNAUTHORIZED, "provider_unavailable"),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
    ] {
        let base_url = spawn_test_server(Router::new().route(
            "/responses",
            post(move || async move { (status, "upstream failure").into_response() }),
        ))
        .await;
        let fixture = ProviderFixture::openai("secret-native-search-key");
        let mut config = fixture.config.clone();
        config.base_url = base_url;
        let provider = OpenAiProvider::from_runtime_config(
            &config,
            "gpt-search",
            1024,
            fixture.home.path(),
            TEST_CLIENT_VERSION,
        )
        .unwrap();

        let failure = provider
            .native_web_search(
                openai_native_search_request(
                    "openai-search",
                    "openai-search/gpt-search",
                    "web_search_preview",
                    "openai_web_search",
                ),
                "query",
            )
            .await
            .expect_err("upstream failure must surface as a typed failure");
        assert_eq!(failure.error_kind, expected_kind);
        assert!(
            !failure.message.contains("secret-native-search-key"),
            "failure message must not leak credentials: {}",
            failure.message
        );
    }
}

#[test]
fn native_search_transport_failure_maps_web_search_error_kinds() {
    let cases = [
        (ProviderFailureKind::Timeout, "timeout"),
        (ProviderFailureKind::RateLimited, "rate_limited"),
        (ProviderFailureKind::AuthError, "provider_unavailable"),
        (ProviderFailureKind::PaymentRequired, "provider_unavailable"),
        (
            ProviderFailureKind::CredentialRefreshBusy,
            "provider_unavailable",
        ),
        (ProviderFailureKind::Connection, "network_failed"),
        (ProviderFailureKind::ServerError, "network_failed"),
        (ProviderFailureKind::Unknown, "network_failed"),
    ];
    for (kind, expected) in cases {
        let error = provider_transport_error(
            ProviderFailureClassification {
                kind,
                disposition: RetryDisposition::FailFast,
            },
            Some(500),
            None,
            "transport failed",
        );
        let failure = native_search_transport_failure(error);
        assert_eq!(failure.error_kind, expected);
        assert!(failure.message.ends_with("transport failed"));
    }
}

#[tokio::test]
async fn codex_native_web_search_routes_codex_transport_with_oauth_credential() {
    let captured_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_server = captured_bodies.clone();
    let captured_headers = Arc::new(Mutex::new(Vec::<HeaderMap>::new()));
    let captured_headers_for_server = captured_headers.clone();
    let base_url = spawn_test_server(Router::new().route(
        "/codex/responses",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let captured = captured_for_server.clone();
            let captured_headers = captured_headers_for_server.clone();
            async move {
                captured.lock().unwrap().push(body);
                captured_headers.lock().unwrap().push(headers);
                (
                    [("content-type", "text/event-stream")],
                    native_search_sse_response("resp_codex_native_1"),
                )
            }
        }),
    ))
    .await;
    let mut fixture = ProviderFixture::codex();
    fixture.config.base_url = base_url;
    let provider = codex_provider(&fixture, 2048);

    let outcome = provider
        .native_web_search(
            openai_native_search_request(
                "openai-codex-search",
                "openai-codex/gpt-codex",
                "web_search",
                "openai_codex_web_search",
            ),
            "codex standalone query",
        )
        .await
        .expect("codex native web search should succeed");

    assert!(outcome.search_executed);
    assert_eq!(
        outcome
            .citations
            .iter()
            .map(|citation| citation.url.as_str())
            .collect::<Vec<_>>(),
        ["https://example.com/first", "https://example.com/second"]
    );

    let headers = captured_headers.lock().unwrap();
    assert_eq!(headers.len(), 1);
    let authorization = headers[0]
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .expect("authorization header");
    assert!(
        authorization.starts_with("Bearer "),
        "codex native search must authenticate with the OAuth bearer token"
    );
    assert_eq!(
        headers[0]
            .get("chatgpt-account-id")
            .and_then(|value| value.to_str().ok()),
        Some("acct_test")
    );
    assert_eq!(
        headers[0]
            .get("originator")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs")
    );

    let bodies = captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    assert_eq!(body["model"], json!("gpt-5.4"));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["tools"], json!([{ "type": "web_search" }]));
    let input = body["input"].as_array().expect("input should be an array");
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], json!("user"));
    assert_eq!(
        input[0]["content"][0]["text"],
        json!("codex standalone query")
    );
    assert!(
        body.get("max_output_tokens").is_none(),
        "codex streaming contract must not send max_output_tokens"
    );
}

#[tokio::test]
async fn codex_native_web_search_classifies_auth_and_rate_limit_failures() {
    for (status, expected_kind) in [
        (StatusCode::UNAUTHORIZED, "provider_unavailable"),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
    ] {
        let base_url = spawn_test_server(Router::new().route(
            "/codex/responses",
            post(move || async move { (status, "upstream failure").into_response() }),
        ))
        .await;
        let mut fixture = ProviderFixture::codex();
        fixture.config.base_url = base_url;
        let provider = codex_provider(&fixture, 2048);

        let failure = provider
            .native_web_search(
                openai_native_search_request(
                    "openai-codex-search",
                    "openai-codex/gpt-codex",
                    "web_search",
                    "openai_codex_web_search",
                ),
                "query",
            )
            .await
            .expect_err("upstream failure must surface as a typed failure");
        assert_eq!(failure.error_kind, expected_kind);
    }
}

fn native_search_response_without_answer_text(response_id: &str) -> Value {
    json!({
        "id": response_id,
        "status": "completed",
        "usage": { "input_tokens": 9, "output_tokens": 2 },
        "output": [
            { "type": "web_search_call", "id": "ws_1", "status": "completed" },
            {
                "type": "message",
                "id": "msg_1",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "   ",
                    "annotations": [
                        {
                            "type": "url_citation",
                            "url": "https://example.com/page#frag-a",
                            "title": "Fragment A"
                        },
                        {
                            "type": "url_citation",
                            "url": "https://example.com/page#frag-b"
                        }
                    ]
                }]
            }
        ]
    })
}

#[tokio::test]
async fn openai_native_web_search_accepts_search_call_only_response() {
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || async {
            Json(json!({
                "id": "resp_search_only",
                "status": "completed",
                "usage": { "input_tokens": 6, "output_tokens": 1 },
                "output": [
                    { "type": "web_search_call", "id": "ws_1", "status": "completed" }
                ]
            }))
        }),
    ))
    .await;
    let fixture = ProviderFixture::openai("native-search-key");
    let mut config = fixture.config.clone();
    config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &config,
        "gpt-search",
        1024,
        fixture.home.path(),
        TEST_CLIENT_VERSION,
    )
    .unwrap();

    let outcome = provider
        .native_web_search(
            openai_native_search_request(
                "openai-search",
                "openai-search/gpt-search",
                "web_search_preview",
                "openai_web_search",
            ),
            "standalone query",
        )
        .await
        .expect("search-call-only response is a successful standalone search");

    assert!(outcome.search_executed);
    assert!(outcome.citations.is_empty());
}

#[tokio::test]
async fn openai_native_web_search_accepts_citations_without_answer_text() {
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || async { Json(native_search_response_without_answer_text("resp_cite_only")) }),
    ))
    .await;
    let fixture = ProviderFixture::openai("native-search-key");
    let mut config = fixture.config.clone();
    config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &config,
        "gpt-search",
        1024,
        fixture.home.path(),
        TEST_CLIENT_VERSION,
    )
    .unwrap();

    let outcome = provider
        .native_web_search(
            openai_native_search_request(
                "openai-search",
                "openai-search/gpt-search",
                "web_search_preview",
                "openai_web_search",
            ),
            "standalone query",
        )
        .await
        .expect("citations without answer text are a successful standalone search");

    assert!(outcome.search_executed);
    let urls: Vec<&str> = outcome
        .citations
        .iter()
        .map(|citation| citation.url.as_str())
        .collect();
    assert_eq!(
        urls,
        [
            "https://example.com/page#frag-a",
            "https://example.com/page#frag-b"
        ]
    );
    assert_eq!(outcome.citations[0].title.as_deref(), Some("Fragment A"));
    assert_eq!(outcome.citations[1].title, None);
}

#[tokio::test]
async fn codex_native_web_search_accepts_completed_search_without_answer_text() {
    let sse_body = format!(
        "event: response.completed\ndata: {}\n\n",
        json!({
            "type": "response.completed",
            "response": native_search_response_without_answer_text("resp_codex_cite_only"),
        })
    );
    let base_url = spawn_test_server(Router::new().route(
        "/codex/responses",
        post(move || {
            let body = sse_body.clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    ))
    .await;
    let mut fixture = ProviderFixture::codex();
    fixture.config.base_url = base_url;
    let provider = codex_provider(&fixture, 2048);

    let outcome = provider
        .native_web_search(
            openai_native_search_request(
                "openai-codex-search",
                "openai-codex/gpt-codex",
                "web_search",
                "openai_codex_web_search",
            ),
            "codex standalone query",
        )
        .await
        .expect("completed search without answer text is a successful standalone search");

    assert!(outcome.search_executed);
    let urls: Vec<&str> = outcome
        .citations
        .iter()
        .map(|citation| citation.url.as_str())
        .collect();
    assert_eq!(
        urls,
        [
            "https://example.com/page#frag-a",
            "https://example.com/page#frag-b"
        ]
    );
}

#[tokio::test]
async fn native_web_search_failures_redact_credentials_and_private_endpoints() {
    let base_url = spawn_test_server(Router::new().route(
        "/responses",
        post(move || async {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": "upstream rejected key sk-live-abcdef123456 for http://10.1.2.3:9000/v1/responses",
                        "type": "server_error"
                    }
                })),
            )
        }),
    ))
    .await;
    let fixture = ProviderFixture::openai("native-search-key");
    let mut config = fixture.config.clone();
    config.base_url = base_url;
    let provider = OpenAiProvider::from_runtime_config(
        &config,
        "gpt-search",
        1024,
        fixture.home.path(),
        TEST_CLIENT_VERSION,
    )
    .unwrap();

    let failure = provider
        .native_web_search(
            openai_native_search_request(
                "openai-search",
                "openai-search/gpt-search",
                "web_search_preview",
                "openai_web_search",
            ),
            "query",
        )
        .await
        .expect_err("upstream failure must surface as a typed failure");

    assert_eq!(failure.error_kind, "network_failed");
    assert!(
        !failure.message.contains("sk-live-abcdef123456"),
        "failure message must not leak credentials: {}",
        failure.message
    );
    assert!(
        !failure.message.contains("10.1.2.3"),
        "failure message must not leak private endpoints: {}",
        failure.message
    );
    assert!(
        failure.message.contains("[REDACTED]"),
        "sanitized detail keeps the diagnosable shape: {}",
        failure.message
    );
}
