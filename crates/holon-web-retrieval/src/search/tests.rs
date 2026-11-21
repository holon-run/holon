use super::*;
use crate::{test_tool_error, WebSearchConfig};
use axum::{
    body::Body,
    http::{
        header::{CONTENT_ENCODING, CONTENT_TYPE},
        HeaderValue,
    },
    response::Response,
};
use flate2::{write::GzEncoder, Compression};
use std::sync::{Arc, Mutex};
use std::{collections::BTreeMap, io::Write};

async fn search(request: WebSearchRequest, config: &WebConfig) -> Result<WebSearchResponse> {
    super::search(test_tool_error, request, config).await
}

fn routing_request(provider: Option<&str>) -> WebSearchRequest {
    WebSearchRequest {
        query: "fixture query".to_string(),
        max_results: Some(2),
        provider: provider.map(str::to_string),
    }
}

type RoutingCalls = Arc<Mutex<Vec<&'static str>>>;

async fn routing_fixture() -> (WebConfig, RoutingCalls) {
    let calls = RoutingCalls::default();
    let mut router = axum::Router::new();
    let fixtures = [
        (
            "first",
            StatusCode::OK,
            searxng_results_json(&[("Shared", "https://example.test/shared#first", "")]),
        ),
        (
            "second",
            StatusCode::OK,
            searxng_results_json(&[
                ("Duplicate", "https://example.test/shared#second", ""),
                ("Unique", "https://example.test/unique", ""),
            ]),
        ),
        ("broken", StatusCode::INTERNAL_SERVER_ERROR, json!({})),
        ("empty", StatusCode::OK, searxng_results_json(&[])),
    ];
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut providers = Vec::new();
    for (id, status, body) in fixtures {
        let calls = Arc::clone(&calls);
        router = router.route(
            &format!("/{id}/search"),
            axum::routing::get(move || {
                let body = body.clone();
                async move {
                    calls.lock().unwrap().push(id);
                    (status, axum::Json(body))
                }
            }),
        );
        providers.push((
            id,
            test_provider(WebProviderKind::Searxng, &format!("http://{addr}/{id}/")),
        ));
    }
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (
        test_search_config(
            providers,
            vec!["first", "second", "broken"],
            WebSearchMode::Single,
        ),
        calls,
    )
}

fn take_routing_calls(calls: &RoutingCalls) -> Vec<&'static str> {
    std::mem::take(&mut *calls.lock().unwrap())
}

fn attempted_ids(response: &WebSearchResponse) -> Vec<&str> {
    response
        .provider_attempts
        .iter()
        .map(|attempt| attempt.provider.as_str())
        .collect()
}

#[tokio::test]
async fn listed_fixture_ids_are_callable_individually_without_enumeration_requests() {
    let (mut config, calls) = routing_fixture().await;
    config
        .providers
        .retain(|id, _| matches!(id.as_str(), "first" | "second"));
    config.search.providers = vec!["first".into()];
    config.search.max_provider_attempts = 1;
    config.search.mode = WebSearchMode::Aggregate;

    let ids = list_search_providers(&config);
    assert_eq!(ids, ["duckduckgo", "first", "second"]);
    assert!(take_routing_calls(&calls).is_empty());

    for id in ids.iter().filter(|id| *id != "duckduckgo") {
        let response = search(routing_request(Some(id)), &config).await.unwrap();
        assert_eq!(response.mode, WebSearchMode::Single);
        assert_eq!(attempted_ids(&response), [id.as_str()]);
        assert_eq!(take_routing_calls(&calls), [id.as_str()]);
    }
}

#[test]
fn discovery_filters_unaddressable_or_incomplete_providers() {
    const FIXTURE_URL: &str = "http://127.0.0.1:1";
    let searxng = test_provider(WebProviderKind::Searxng, FIXTURE_URL);
    let missing_endpoint = WebProviderConfig {
        base_url: None,
        ..searxng.clone()
    };
    let mut missing_key = test_provider(WebProviderKind::Brave, FIXTURE_URL);
    missing_key.api_key.clear();
    let mut public_keenable = test_provider(WebProviderKind::Keenable, FIXTURE_URL);
    public_keenable.api_key.clear();
    let missing_mapping = WebProviderConfig {
        output: None,
        ..command_test_provider(vec!["unused-fixture".into()])
    };
    let mut config = test_search_config(
        vec![
            ("keenable", public_keenable),
            (
                "deepseek",
                test_provider(WebProviderKind::DeepSeek, FIXTURE_URL),
            ),
            ("broken-brave", missing_key),
            ("no-base-url", missing_endpoint),
            (
                "bad-base-url",
                test_provider(WebProviderKind::Tavily, "not a URL"),
            ),
            ("missing-mapping", missing_mapping),
            ("empty-argv", command_test_provider(vec![])),
            ("auto", searxng.clone()),
            ("aggregate", searxng.clone()),
            (" spaced ", searxng),
            (
                "conversation-only",
                test_provider(WebProviderKind::OpenAiNative, FIXTURE_URL),
            ),
            (
                "duckduckgo",
                test_provider(WebProviderKind::Searxng, FIXTURE_URL),
            ),
        ],
        vec!["auto"],
        WebSearchMode::Fallback,
    );
    assert_eq!(
        list_search_providers(&config),
        ["deepseek", "duckduckgo", "keenable"]
    );
    config.search.enabled = false;
    assert!(list_search_providers(&config).is_empty());
}

#[test]
fn discovery_lists_native_providers_only_with_bound_model_route() {
    let mut reserved_anthropic =
        test_provider(WebProviderKind::AnthropicNative, "https://reserved.example");
    reserved_anthropic.model_route = Some("anthropic@default/claude-test".into());
    let mut reserved_gemini =
        test_provider(WebProviderKind::GeminiNative, "https://reserved.example");
    reserved_gemini.model_route = Some("gemini@default/gemini-test".into());
    let config = test_search_config(
        vec![
            (
                "openai-search",
                native_provider_config(Some("openai@default/gpt-test")),
            ),
            ("unrouted-native", native_provider_config(None)),
            ("reserved-anthropic", reserved_anthropic),
            ("reserved-gemini", reserved_gemini),
        ],
        vec![],
        WebSearchMode::Single,
    );
    assert_eq!(
        list_search_providers(&config),
        ["duckduckgo", "openai-search"]
    );
}

#[cfg(unix)]
#[test]
fn discovery_does_not_start_command_providers() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_MARKER: AtomicUsize = AtomicUsize::new(0);
    let marker = std::env::temp_dir().join(format!(
        "holon-search-discovery-{}-{}",
        std::process::id(),
        NEXT_MARKER.fetch_add(1, Ordering::Relaxed)
    ));
    let config = test_search_config(
        vec![(
            "command-fixture",
            command_test_provider(vec!["touch".into(), marker.to_str().unwrap().into()]),
        )],
        vec![],
        WebSearchMode::Single,
    );
    assert_eq!(
        list_search_providers(&config),
        ["command-fixture", "duckduckgo"]
    );
    assert!(!marker.exists());
}

#[tokio::test]
async fn omitted_and_auto_selectors_keep_configured_mode_and_order() {
    let (mut config, calls) = routing_fixture().await;
    use WebSearchMode::{Aggregate, Fallback, Single};
    for (mode, named_default, named_calls, auto_calls) in [
        (Single, "second", &["second"][..], &["first"][..]),
        (Fallback, "broken", &["broken", "empty"][..], &["first"][..]),
        (
            Aggregate,
            "second",
            &["second"][..],
            &["first", "second"][..],
        ),
    ] {
        config.search.mode = mode;
        for (default_provider, selector, expected_calls) in [
            ("auto", None, auto_calls),
            ("auto", Some("auto"), auto_calls),
            (named_default, None, named_calls),
            (named_default, Some("auto"), auto_calls),
        ] {
            config.search.provider = default_provider.to_string();

            let result = search(routing_request(selector), &config).await.unwrap();
            assert_eq!(result.mode, mode);
            assert_eq!(take_routing_calls(&calls), expected_calls);
            assert_eq!(attempted_ids(&result), expected_calls);
            let winner = (mode != Aggregate).then(|| *expected_calls.last().unwrap());
            assert_eq!(result.winning_provider.as_deref(), winner);
            assert_eq!(
                result.provider,
                winner.or(selector).unwrap_or(default_provider)
            );
        }
    }
}

#[tokio::test]
async fn named_provider_failure_does_not_query_next_provider() {
    let (mut config, calls) = routing_fixture().await;
    config.search.mode = WebSearchMode::Fallback;
    let error = search(routing_request(Some("broken")), &config)
        .await
        .unwrap_err();
    assert_eq!(take_routing_calls(&calls), ["broken"]);
    let details = test_tool_error(&error).details.unwrap();
    assert_eq!(details["provider_attempts"].as_array().unwrap().len(), 1);
    assert_eq!(details["provider_attempts"][0]["provider"], "broken");
}

#[tokio::test]
async fn explicit_aggregate_overrides_single_once_and_stops_at_result_limit() {
    let (mut config, calls) = routing_fixture().await;
    config.search.provider = "second".to_string();

    let result = search(routing_request(Some("aggregate")), &config)
        .await
        .unwrap();
    assert_eq!(result.mode, WebSearchMode::Aggregate);
    assert_eq!(result.provider, "aggregate");
    assert_eq!(result.winning_provider, None);
    assert_eq!(attempted_ids(&result), ["first", "second"]);
    assert_eq!(take_routing_calls(&calls), ["first", "second"]);
    assert_eq!(
        result
            .results
            .iter()
            .map(|row| (row.source.as_str(), row.url.as_str()))
            .collect::<Vec<_>>(),
        [
            ("first", "https://example.test/shared#first"),
            ("second", "https://example.test/unique")
        ]
    );

    let result = search(routing_request(None), &config).await.unwrap();
    assert_eq!(result.mode, WebSearchMode::Single);
    assert_eq!(result.provider, "second");
    assert_eq!(attempted_ids(&result), ["second"]);
    assert_eq!(take_routing_calls(&calls), ["second"]);
}

#[tokio::test]
async fn explicit_aggregate_respects_enable_switch_and_provider_attempt_cap() {
    let (mut config, calls) = routing_fixture().await;
    config.search.max_provider_attempts = 1;
    let result = search(routing_request(Some("aggregate")), &config)
        .await
        .unwrap();
    assert_eq!(result.results.len(), 1);
    assert_eq!(attempted_ids(&result), ["first"]);
    assert_eq!(take_routing_calls(&calls), ["first"]);

    config.search.providers = vec!["broken".into(), "second".into()];
    let error = search(routing_request(Some("aggregate")), &config)
        .await
        .unwrap_err();
    assert_eq!(take_routing_calls(&calls), ["broken"]);
    let error = test_tool_error(&error);
    assert_eq!(error.kind, "provider_unavailable");
    assert_eq!(
        error.details.unwrap()["attempted_providers"],
        json!(["broken"])
    );

    config.search.enabled = false;
    let error = search(routing_request(Some("aggregate")), &config)
        .await
        .unwrap_err();
    assert!(take_routing_calls(&calls).is_empty());
    let error = test_tool_error(&error);
    assert_eq!(error.kind, "provider_unavailable");
    assert_eq!(error.details.unwrap()["provider"], "disabled");
}

#[tokio::test]
async fn explicit_aggregate_exhaustion_retains_errors_and_attempt_order() {
    let (mut config, calls) = routing_fixture().await;
    config.search.providers = vec!["empty".into(), "broken".into()];
    let error = search(routing_request(Some("aggregate")), &config)
        .await
        .unwrap_err();

    assert_eq!(take_routing_calls(&calls), ["empty", "broken"]);
    let error = test_tool_error(&error);
    assert_eq!(error.kind, "provider_unavailable");
    let details = error.details.unwrap();
    assert_eq!(details["attempted_providers"], json!(["empty", "broken"]));
    assert_eq!(details["winning_provider"], Value::Null);
    let attempts = details["provider_attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    for (attempt, (provider, status)) in attempts
        .iter()
        .zip([("empty", "success"), ("broken", "error")])
    {
        assert_eq!(attempt["provider"], provider);
        assert_eq!(attempt["status"], status);
    }
}

#[tokio::test]
async fn registered_aliases_are_rejected_only_as_explicit_selectors() {
    let (mut config, calls) = routing_fixture().await;
    for alias in ["auto", "aggregate"] {
        config
            .providers
            .insert(alias.into(), config.providers["first"].clone());
        config.search.providers = vec![alias.into(), "second".into()];
        config.search.provider = alias.into();

        let error = search(routing_request(Some(alias)), &config)
            .await
            .unwrap_err();
        assert!(take_routing_calls(&calls).is_empty());
        let error = test_tool_error(&error);
        assert_eq!(error.kind, "invalid_tool_input");
        assert_eq!(error.details.unwrap()["provider"], alias);
        assert!(error.recovery_hint.unwrap().contains("omit"));

        let result = search(routing_request(None), &config).await.unwrap();
        assert_eq!(result.provider, alias);
        assert_eq!(attempted_ids(&result), [alias]);
        assert_eq!(take_routing_calls(&calls), ["first"]);
    }
}

#[test]
fn duckduckgo_lite_parses_result_roles_and_deduplicates_canonical_urls() {
    let html = r#"
        <table>
            <tr><td><a rel="nofollow" href="https://example.com/docs">More at Example</a></td></tr>
            <tr><td><a href='//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs%23intro&amp;rut=x'
                       class='result-link highlighted' rel='nofollow'>Example &amp; <b>Docs</b></a></td></tr>
            <tr><td><A CLASS="result-link" HREF="https://EXAMPLE.com:443/docs#details">Duplicate</A></td></tr>
            <tr><td><a class=result-link href="javascript:void(0)">Not a web URL</a></td></tr>
            <tr><td><a class=result-link href="/l/?uddg=not-a-url">Broken wrapper</a></td></tr>
            <tr><td><a class=result-link href="/l/?uddg=https%3A%2F%2Fexample.org%2Fguide">Guide &#8212; Rust</a></td></tr>
            <tr><td><a class=result-link href="https://example.net/search?uddg=literal&amp;q=rust">Direct result</a></td></tr>
            <tr><td><a rel="nofollow" href="https://example.com/navigation">Other navigation</a></td></tr>
        </table>
    "#;
    let results = parse_duckduckgo_lite_results(html, 5).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].title, "Example & Docs");
    assert_eq!(results[0].url, "https://example.com/docs#intro");
    assert_eq!(
        results[0].canonical_url.as_deref(),
        Some("https://example.com/docs")
    );
    assert_eq!(results[1].title, "Guide — Rust");
    assert_eq!(results[1].url, "https://example.org/guide");
    assert_eq!(
        results[2].url,
        "https://example.net/search?uddg=literal&q=rust"
    );
    assert_eq!(
        results.iter().map(|result| result.rank).collect::<Vec<_>>(),
        [Some(1), Some(2), Some(3)]
    );
    let limited = parse_duckduckgo_lite_results(html, 2).unwrap();
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[1].url, "https://example.org/guide");
}

#[test]
fn duckduckgo_lite_distinguishes_bot_challenge_from_unrecognized_markup() {
    // A successful HTTP response can carry a challenge form instead of results.
    let challenge = r#"
        <form action="//duckduckgo.com/anomaly.js" method=POST id=challenge-form></form>
    "#;
    let unexpected = r#"
        <!-- <form id="challenge-form"></form> -->
        <a rel="nofollow" href="https://example.com/">More at Example</a>
    "#;
    for (html, kind, message) in [
        (challenge, "provider_blocked", "anti-bot challenge"),
        (unexpected, "parse_failed", "markup may have changed"),
    ] {
        let error = test_tool_error(&parse_duckduckgo_lite_results(html, 5).unwrap_err());
        assert_eq!(error.kind, kind);
        assert!(!error.retryable);
        assert!(error.message.contains(message));
        assert_eq!(error.details.unwrap()["provider"], "duckduckgo");
        assert!(error.recovery_hint.unwrap().contains("another"));
    }
}

#[test]
fn searxng_search_url_preserves_path_prefix() {
    assert_eq!(
        searxng_search_url("https://example.com/searxng/")
            .unwrap()
            .as_str(),
        "https://example.com/searxng/search"
    );
    assert_eq!(
        searxng_search_url("https://example.com/searxng")
            .unwrap()
            .as_str(),
        "https://example.com/searxng/search"
    );
}

#[test]
fn max_results_zero_is_invalid() {
    let error = normalize_max_results(Some(0), 5).unwrap_err();
    let tool_error = crate::test_tool_error(&error);
    assert_eq!(tool_error.kind, "invalid_tool_input");
}

#[test]
fn normalize_search_result_populates_standard_fields() {
    let mut metadata = BTreeMap::new();
    metadata.insert("engine".to_string(), json!("mock"));
    let result = normalize_search_result(
        "mock_provider",
        2,
        ProviderRawSearchResult {
            title: "  Example Result  ".to_string(),
            url: "https://example.com/docs?q=holon#section".to_string(),
            snippet: Some("  useful context  ".to_string()),
            published_at: Some(" 2026-07-07 ".to_string()),
            language: Some(" en ".to_string()),
            region: Some(" US ".to_string()),
            metadata,
            ..ProviderRawSearchResult::default()
        },
    )
    .unwrap();

    assert_eq!(result.title, "Example Result");
    assert_eq!(result.url, "https://example.com/docs?q=holon#section");
    assert_eq!(result.snippet.as_deref(), Some("useful context"));
    assert_eq!(result.source, "mock_provider");
    assert_eq!(
        result.canonical_url.as_deref(),
        Some("https://example.com/docs?q=holon")
    );
    assert_eq!(
        result.display_url.as_deref(),
        Some("example.com/docs?q=holon")
    );
    assert_eq!(result.rank, Some(2));
    assert_eq!(result.published_at.as_deref(), Some("2026-07-07"));
    assert_eq!(result.language.as_deref(), Some("en"));
    assert_eq!(result.region.as_deref(), Some("US"));
    assert_eq!(
        result.metadata.as_ref().unwrap().get("engine"),
        Some(&json!("mock"))
    );
}

#[tokio::test]
async fn fallback_mode_tries_explicit_order_until_success() {
    let good_url = searxng_mock_base_url(searxng_results_json(&[(
        "Good result",
        "https://example.com/good",
        "ok",
    )]))
    .await;
    let config = test_search_config(
        vec![
            (
                "bad",
                WebProviderConfig {
                    kind: WebProviderKind::Searxng,
                    base_url: None,
                    api_key: String::new(),
                    model_route: None,
                    command: None,
                    output: None,
                    limits: Default::default(),
                },
            ),
            ("good", test_provider(WebProviderKind::Searxng, &good_url)),
        ],
        vec!["bad", "good"],
        WebSearchMode::Fallback,
    );

    let response = search(
        WebSearchRequest {
            query: "test".to_string(),
            max_results: Some(5),
            provider: None,
        },
        &config,
    )
    .await
    .unwrap();

    assert_eq!(response.mode, WebSearchMode::Fallback);
    assert_eq!(response.winning_provider.as_deref(), Some("good"));
    assert_eq!(response.provider_attempts.len(), 2);
    assert_eq!(response.provider_attempts[0].provider, "bad");
    assert_eq!(
        response.provider_attempts[0].status,
        WebSearchProviderAttemptStatus::Error
    );
    assert_eq!(response.provider_attempts[1].provider, "good");
    assert_eq!(
        response.provider_attempts[1].status,
        WebSearchProviderAttemptStatus::Success
    );
    assert_eq!(
        response.provider_attempts[1].provider_kind,
        Some(WebProviderKind::Searxng)
    );
    assert_eq!(
        response.provider_attempts[1].capabilities.unwrap().status,
        crate::WebProviderSupportStatus::Supported
    );
    assert_eq!(response.results[0].source, "good");
}

#[test]
fn provider_order_deduplicates_explicit_auto_order() {
    let config = test_search_config(
        vec![(
            "good",
            test_provider(WebProviderKind::Searxng, "https://good.example"),
        )],
        vec![" good ", "bad", "good", "", " bad "],
        WebSearchMode::Fallback,
    );

    assert_eq!(provider_order("auto", &config), vec!["good", "bad"]);
}

#[test]
fn provider_order_defaults_to_configured_providers() {
    let config = test_search_config(
        vec![
            (
                "zeta",
                test_provider(WebProviderKind::Searxng, "https://zeta.example"),
            ),
            (
                "alpha",
                test_provider(WebProviderKind::Searxng, "https://alpha.example"),
            ),
        ],
        vec![],
        WebSearchMode::Fallback,
    );

    assert_eq!(
        provider_order("auto", &config),
        vec!["alpha", "zeta", FIRECRAWL_PROVIDER_ID]
    );
}

#[test]
fn provider_order_defaults_skip_unsupported_configured_providers() {
    let config = test_search_config(
        vec![
            (
                "future",
                test_provider(WebProviderKind::GeminiNative, "https://future.example"),
            ),
            (
                "native",
                test_provider(WebProviderKind::OpenAiNative, "https://native.example"),
            ),
            (
                "searx",
                test_provider(WebProviderKind::Searxng, "https://searx.example"),
            ),
        ],
        vec![],
        WebSearchMode::Fallback,
    );

    assert_eq!(
        provider_order("auto", &config),
        vec!["searx", FIRECRAWL_PROVIDER_ID, KEENABLE_PROVIDER_ID]
    );
}

#[test]
fn provider_order_defaults_to_capability_priority() {
    let config = test_search_config(
        vec![
            (
                "searx",
                test_provider(WebProviderKind::Searxng, "https://searx.example"),
            ),
            (
                "exa",
                test_provider(WebProviderKind::Exa, "https://exa.example"),
            ),
            (
                "brave",
                test_provider(WebProviderKind::Brave, "https://brave.example"),
            ),
            (
                "tavily",
                test_provider(WebProviderKind::Tavily, "https://tavily.example"),
            ),
        ],
        vec![],
        WebSearchMode::Fallback,
    );

    assert_eq!(
        provider_order("auto", &config),
        vec!["brave", "tavily", "exa"]
    );
}

#[test]
fn provider_order_defaults_to_keyless_builtin_chain() {
    let config = test_search_config(vec![], vec![], WebSearchMode::Fallback);

    assert_eq!(
        provider_order("auto", &config),
        vec![
            FIRECRAWL_PROVIDER_ID,
            KEENABLE_PROVIDER_ID,
            DUCKDUCKGO_PROVIDER_ID
        ]
    );
}

#[test]
fn provider_order_builtin_chain_skips_configured_ids() {
    let config = test_search_config(
        vec![(
            FIRECRAWL_PROVIDER_ID,
            test_provider(WebProviderKind::Firecrawl, "https://firecrawl.example"),
        )],
        vec![],
        WebSearchMode::Fallback,
    );

    assert_eq!(
        provider_order("auto", &config),
        vec![
            FIRECRAWL_PROVIDER_ID,
            KEENABLE_PROVIDER_ID,
            DUCKDUCKGO_PROVIDER_ID
        ]
    );
}

#[test]
fn builtin_keyless_provider_kinds_and_configs() {
    assert_eq!(
        builtin_keyless_provider_kind(FIRECRAWL_PROVIDER_ID),
        Some(WebProviderKind::Firecrawl)
    );
    assert_eq!(
        builtin_keyless_provider_kind(KEENABLE_PROVIDER_ID),
        Some(WebProviderKind::Keenable)
    );
    assert_eq!(builtin_keyless_provider_kind("other"), None);

    let firecrawl = builtin_keyless_provider_config(FIRECRAWL_PROVIDER_ID).unwrap();
    assert_eq!(firecrawl.kind, WebProviderKind::Firecrawl);
    assert!(firecrawl.api_key.is_empty());
    assert!(firecrawl.base_url.is_none());
    assert_eq!(builtin_keyless_provider_config("other"), None);
}

#[tokio::test]
async fn single_provider_request_does_not_fallback() {
    let good_url = searxng_mock_base_url(searxng_results_json(&[(
        "Good result",
        "https://example.com/good",
        "ok",
    )]))
    .await;
    let config = test_search_config(
        vec![
            (
                "bad",
                WebProviderConfig {
                    kind: WebProviderKind::Searxng,
                    base_url: None,
                    api_key: String::new(),
                    model_route: None,
                    command: None,
                    output: None,
                    limits: Default::default(),
                },
            ),
            ("good", test_provider(WebProviderKind::Searxng, &good_url)),
        ],
        vec!["bad", "good"],
        WebSearchMode::Fallback,
    );

    let err = search(
        WebSearchRequest {
            query: "test".to_string(),
            max_results: Some(5),
            provider: Some("bad".to_string()),
        },
        &config,
    )
    .await
    .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "provider_unavailable");
    assert_eq!(tool_error.message, "SearXNG provider requires base_url");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["provider"], json!("bad"));
    assert_eq!(details["attempted_providers"], json!(["bad"]));
    assert_eq!(details["winning_provider"], serde_json::Value::Null);
    assert_eq!(details["provider_attempts"].as_array().unwrap().len(), 1);
    assert_eq!(
        details["provider_attempts"][0]["provider_kind"],
        json!("searxng")
    );
    assert_eq!(
        details["provider_attempts"][0]["capabilities"]["status"],
        json!("supported")
    );
}

#[tokio::test]
async fn aggregate_mode_deduplicates_urls_and_keeps_provenance() {
    let first_url = searxng_mock_base_url(searxng_results_json(&[
        ("Shared", "https://example.com/shared#one", "from one"),
        ("One", "https://example.com/one", "only one"),
    ]))
    .await;
    let second_url = searxng_mock_base_url(searxng_results_json(&[
        ("Shared", "https://example.com/shared#two", "from two"),
        ("Two", "https://example.com/two", "only two"),
    ]))
    .await;
    let config = test_search_config(
        vec![
            ("one", test_provider(WebProviderKind::Searxng, &first_url)),
            ("two", test_provider(WebProviderKind::Searxng, &second_url)),
        ],
        vec!["one", "two"],
        WebSearchMode::Aggregate,
    );

    let response = search(
        WebSearchRequest {
            query: "test".to_string(),
            max_results: Some(5),
            provider: None,
        },
        &config,
    )
    .await
    .unwrap();

    assert_eq!(response.mode, WebSearchMode::Aggregate);
    assert_eq!(response.winning_provider, None);
    assert_eq!(response.provider_attempts.len(), 2);
    assert!(response
        .provider_attempts
        .iter()
        .all(|attempt| attempt.status == WebSearchProviderAttemptStatus::Success));
    assert_eq!(
        response
            .results
            .iter()
            .filter(|result| {
                result.canonical_url.as_deref() == Some("https://example.com/shared")
            })
            .count(),
        1
    );
    assert!(response
        .results
        .iter()
        .any(|result| result.url == "https://example.com/shared#one" && result.source == "one"));
    assert!(response
        .results
        .iter()
        .any(|result| result.url == "https://example.com/two" && result.source == "two"));
}

#[tokio::test]
async fn command_provider_executes_fixed_argv_and_maps_json_results() {
    let provider = WebProviderConfig {
            kind: WebProviderKind::Command,
            base_url: None,
            api_key: String::new(),
            model_route: None,
            command: Some(crate::WebCommandProviderConfig {
                argv: vec![
                    "printf".into(),
                    r#"[{"title":"{{query}}","url":"https://example.com/{{max_results}}","abstract":"Snippet"}]"#.into(),
                ],
            }),
            output: Some(crate::WebCommandOutputConfig {
                format: crate::WebCommandOutputFormat::Json,
                mapping: crate::WebCommandResultMapping {
                    title: ".title".into(),
                    url: ".url".into(),
                    snippet: Some(".abstract".into()),
                    published_at: None,
                },
            }),
            limits: crate::WebProviderLimitsConfig {
                timeout_ms: 5_000,
                max_output_bytes: 10_000,
            },
        };

    let results = command_search(crate::test_tool_error, "holon", 3, "cmd", &provider)
        .await
        .unwrap();

    assert_eq!(results.results.len(), 1);
    assert_eq!(results.results[0].title, "holon");
    assert_eq!(results.results[0].url, "https://example.com/3");
    assert_eq!(results.results[0].snippet.as_deref(), Some("Snippet"));
    assert_eq!(results.results[0].source, "cmd");
    assert_eq!(
        results.results[0].canonical_url.as_deref(),
        Some("https://example.com/3")
    );
    assert_eq!(results.results[0].rank, Some(1));
}

#[tokio::test]
async fn command_provider_exit_failure_is_structured_tool_error() {
    let provider = command_test_provider(vec!["false".into()]);

    let err = command_search(crate::test_tool_error, "holon", 3, "cmd", &provider)
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);

    assert_eq!(tool_error.kind, "provider_unavailable");
    assert!(!tool_error.retryable);
    assert_eq!(
        tool_error.details.as_ref().unwrap()["provider"],
        json!("cmd")
    );
    assert_eq!(
        tool_error.details.as_ref().unwrap()["command"]["argv_template"],
        json!(["false"])
    );
}

#[tokio::test]
async fn command_provider_invalid_json_keeps_command_attempt() {
    let provider = command_test_provider(vec!["printf".into(), "not-json".into()]);

    let err = command_search(crate::test_tool_error, "holon", 3, "cmd", &provider)
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);

    assert_eq!(tool_error.kind, "parse_failed");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["provider"], json!("cmd"));
    assert_eq!(
        details["command"]["argv_template"],
        json!(["printf", "not-json"])
    );
    assert_eq!(details["command"]["exit_status"], json!(0));
}

#[tokio::test]
async fn command_provider_invalid_utf8_keeps_command_attempt() {
    let provider = command_test_provider(vec!["printf".into(), "\\377".into()]);

    let err = command_search(crate::test_tool_error, "holon", 3, "cmd", &provider)
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);

    assert_eq!(tool_error.kind, "parse_failed");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["provider"], json!("cmd"));
    assert_eq!(
        details["command"]["argv_template"],
        json!(["printf", "\\377"])
    );
    assert_eq!(details["command"]["exit_status"], json!(0));
}

#[tokio::test]
async fn command_provider_mapping_failure_keeps_command_attempt() {
    let provider = command_test_provider(vec!["printf".into(), r#"[{"title":"No URL"}]"#.into()]);

    let err = command_search(crate::test_tool_error, "holon", 3, "cmd", &provider)
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);

    assert_eq!(tool_error.kind, "parse_failed");
    let details = tool_error.details.as_ref().unwrap();
    assert_eq!(details["provider"], json!("cmd"));
    assert_eq!(
        details["command"]["argv_template"],
        json!(["printf", r#"[{"title":"No URL"}]"#])
    );
    assert_eq!(details["command"]["exit_status"], json!(0));
}

#[test]
fn command_arg_expansion_does_not_reexpand_query_placeholders() {
    assert_eq!(
        expand_command_arg(
            "q={{query}}&n={{max_results}}",
            "literal {{max_results}}",
            7
        ),
        "q=literal {{max_results}}&n=7"
    );
}

#[test]
fn command_result_mapping_reads_nested_paths() {
    let entry = json!({
        "meta": { "title": "Nested" },
        "url": "https://example.com"
    });

    assert_eq!(
        mapped_json_string(&entry, ".meta.title").as_deref(),
        Some("Nested")
    );
    assert_eq!(
        mapped_json_string(&entry, "url").as_deref(),
        Some("https://example.com")
    );
    assert_eq!(mapped_json_string(&entry, ".missing.title"), None);
}

// ---------------------------------------------------------------------------
// Integration tests against mock HTTP servers for API-backed providers
// ---------------------------------------------------------------------------

fn test_provider(kind: WebProviderKind, base_url: &str) -> WebProviderConfig {
    WebProviderConfig {
        kind,
        base_url: Some(base_url.to_string()),
        api_key: "test-key-123".to_string(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    }
}

fn command_test_provider(argv: Vec<String>) -> WebProviderConfig {
    WebProviderConfig {
        kind: WebProviderKind::Command,
        base_url: None,
        api_key: String::new(),
        model_route: None,
        command: Some(crate::WebCommandProviderConfig { argv }),
        output: Some(crate::WebCommandOutputConfig {
            format: crate::WebCommandOutputFormat::Json,
            mapping: crate::WebCommandResultMapping {
                title: ".title".into(),
                url: ".url".into(),
                snippet: None,
                published_at: None,
            },
        }),
        limits: Default::default(),
    }
}

fn test_fetch_config() -> WebFetchConfig {
    WebFetchConfig::default()
}

fn test_search_config(
    providers: Vec<(&str, WebProviderConfig)>,
    order: Vec<&str>,
    mode: WebSearchMode,
) -> WebConfig {
    WebConfig {
        fetch: test_fetch_config(),
        search: WebSearchConfig {
            mode,
            providers: order.into_iter().map(str::to_string).collect(),
            ..WebSearchConfig::default()
        },
        providers: providers
            .into_iter()
            .map(|(id, provider)| (id.to_string(), provider))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn searxng_results_json(entries: &[(&str, &str, &str)]) -> serde_json::Value {
    json!({
        "results": entries
            .iter()
            .map(|(title, url, content)| {
                json!({
                    "title": title,
                    "url": url,
                    "content": content,
                })
            })
            .collect::<Vec<_>>()
    })
}

async fn searxng_mock_base_url(results: serde_json::Value) -> String {
    searxng_mock_response_base_url(StatusCode::OK, "application/json", results.to_string()).await
}

async fn searxng_mock_response_base_url(
    status: StatusCode,
    content_type: &'static str,
    body: String,
) -> String {
    let router = axum::Router::new().route(
        "/search",
        axum::routing::get(move || {
            let body = body.clone();
            async move { (status, [(CONTENT_TYPE, content_type)], body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{}", addr)
}

async fn search_mock_searxng(base_url: &str) -> Result<WebSearchResponse> {
    let config = test_search_config(
        vec![(
            "local_searxng",
            test_provider(WebProviderKind::Searxng, base_url),
        )],
        vec!["local_searxng"],
        WebSearchMode::Single,
    );
    search(
        WebSearchRequest {
            query: "test".to_string(),
            max_results: Some(1),
            provider: Some("local_searxng".to_string()),
        },
        &config,
    )
    .await
}

#[tokio::test]
async fn searxng_search_empty_results_succeeds() {
    let base_url = searxng_mock_base_url(json!({
        "results": [],
        "unresponsive_engines": [["engine", "timeout"]],
    }))
    .await;

    let response = search_mock_searxng(&base_url).await.unwrap();
    assert!(response.results.is_empty());
    assert!(response.citations.is_empty());
    assert_eq!(response.summary_text, "0 web results");
    assert_eq!(response.winning_provider.as_deref(), Some("local_searxng"));
    let [attempt] = response.provider_attempts.as_slice() else {
        panic!("expected one provider attempt");
    };
    assert_eq!(attempt.status, WebSearchProviderAttemptStatus::Success);
    assert_eq!(attempt.result_count, 0);
}

#[tokio::test]
async fn searxng_search_invalid_or_unusable_results_fail_parsing() {
    for (body, expected_message) in [
        ("not json", "SearXNG returned invalid JSON:"),
        (
            r#"{"unresponsive_engines":[]}"#,
            "SearXNG returned no parseable search results",
        ),
        (
            r#"{"results":null}"#,
            "SearXNG returned no parseable search results",
        ),
        (
            r#"{"results":{}}"#,
            "SearXNG returned no parseable search results",
        ),
        (
            r#"{"results":[{"title":"No URL"}]}"#,
            "SearXNG returned no parseable search results",
        ),
    ] {
        let base_url =
            searxng_mock_response_base_url(StatusCode::OK, "application/json", body.to_string())
                .await;
        let error = search_mock_searxng(&base_url).await.unwrap_err();
        let error = test_tool_error(&error);
        assert_eq!(error.kind, "parse_failed", "{body}");
        assert!(error.message.starts_with(expected_message), "{error:?}");
        assert!(!error.retryable, "{body}");
    }
}

#[tokio::test]
async fn searxng_search_still_returns_normalized_nonempty_results() {
    let base_url = searxng_mock_base_url(searxng_results_json(&[
        (
            "Example result",
            "https://example.com/result",
            "Result snippet",
        ),
        (
            "Second result",
            "https://example.org/result",
            "Second snippet",
        ),
    ]))
    .await;
    let response = search_mock_searxng(&base_url).await.unwrap();
    let [result] = response.results.as_slice() else {
        panic!("expected one result after applying max_results");
    };
    assert_eq!(result.title, "Example result");
    assert_eq!(result.url, "https://example.com/result");
    assert_eq!(response.citations.len(), 1);
    assert_eq!(response.citations[0].url, result.url);
}

#[tokio::test]
async fn searxng_search_only_hints_about_json_for_http_403() {
    for status in [StatusCode::FORBIDDEN, StatusCode::SERVICE_UNAVAILABLE] {
        let base_url =
            searxng_mock_response_base_url(status, "text/html", "<p>Denied</p>".to_string()).await;
        let error = search_mock_searxng(&base_url).await.unwrap_err();
        let error = test_tool_error(&error);
        assert_eq!(error.kind, "network_failed");
        assert!(error.retryable);
        assert_eq!(
            error.message,
            format!("WebSearch provider `local_searxng` returned HTTP {status}")
        );
        let hint = error.recovery_hint.as_deref().unwrap();
        if status == StatusCode::FORBIDDEN {
            assert!(hint.contains("`search.formats`"));
            assert!(hint.contains("`json`"));
            assert!(hint.contains("one possible cause"));
        } else {
            assert_eq!(hint, "retry later or configure another WebSearch provider");
        }
    }
}

fn brave_results_json() -> serde_json::Value {
    serde_json::json!({
        "web": {
            "results": [
                {
                    "title": "Brave Search",
                    "url": "https://search.brave.com",
                    "description": "Brave Search engine"
                },
                {
                    "title": "Brave Browser",
                    "url": "https://brave.com",
                    "description": "Privacy-focused browser"
                }
            ]
        }
    })
}

fn tencent_cloud_wsa_results_json() -> serde_json::Value {
    serde_json::json!({
        "Pages": [
            r#"{"title":"Tencent Cloud Search","url":"https://cloud.tencent.com/product/wsa","passage":"Tencent Cloud WSA SearchPro","date":"2026-07-01","site":"cloud.tencent.com","score":0.9}"#,
            r#"{"title":"SearchPro Docs","url":"https://cloud.tencent.com/document/api/1806/121821","passage":"API documentation","site":"cloud.tencent.com"}"#
        ]
    })
}

fn empty_tencent_cloud_wsa_results_json() -> serde_json::Value {
    serde_json::json!({ "Pages": [] })
}

fn bocha_results_json() -> serde_json::Value {
    serde_json::json!({
        "data": {
            "webPages": {
                "value": [
                    {
                        "name": "Bocha Search",
                        "url": "https://bochaai.com",
                        "summary": "AI web search API",
                        "datePublished": "2026-07-02",
                        "siteName": "Bocha"
                    },
                    {
                        "name": "Bocha Docs",
                        "url": "https://open.bochaai.com",
                        "snippet": "Bocha API documentation",
                        "language": "zh"
                    }
                ]
            }
        }
    })
}

fn empty_bocha_results_json() -> serde_json::Value {
    serde_json::json!({ "data": { "webPages": { "value": [] } } })
}

fn tavily_results_json() -> serde_json::Value {
    serde_json::json!({
        "results": [
            {
                "title": "Tavily Search",
                "url": "https://tavily.com",
                "content": "AI-powered search API"
            },
            {
                "title": "Tavily Docs",
                "url": "https://docs.tavily.com",
                "content": "Documentation for Tavily API"
            }
        ]
    })
}

fn exa_results_json() -> serde_json::Value {
    serde_json::json!({
        "results": [
            {
                "title": "Exa Search",
                "url": "https://exa.ai",
                "text": "Semantic search engine"
            },
            {
                "title": "Exa Docs",
                "url": "https://docs.exa.ai",
                "text": "Exa API documentation"
            }
        ]
    })
}

fn perplexity_results_json() -> serde_json::Value {
    serde_json::json!({
        "choices": [
            {
                "message": {
                    "content": "Perplexity summarized these search results."
                }
            }
        ],
        "search_results": [
            {
                "title": "Perplexity Search",
                "url": "https://www.perplexity.ai",
                "date": "2026-05-16"
            },
            {
                "title": "Perplexity Docs",
                "url": "https://docs.perplexity.ai"
            }
        ]
    })
}

fn firecrawl_results_json() -> serde_json::Value {
    serde_json::json!({
        "data": [
            {
                "title": "Firecrawl Search",
                "url": "https://www.firecrawl.dev",
                "description": "Search and scrape API"
            },
            {
                "title": "Firecrawl Docs",
                "url": "https://docs.firecrawl.dev",
                "markdown": "Firecrawl API documentation"
            }
        ]
    })
}

fn empty_results_json() -> serde_json::Value {
    serde_json::json!({ "results": [] })
}

fn empty_brave_results_json() -> serde_json::Value {
    serde_json::json!({ "web": { "results": [] } })
}

#[tokio::test]
async fn brave_search_integration_success() {
    let router = axum::Router::new().route(
        "/res/v1/web/search",
        axum::routing::get(|| async { axum::Json(brave_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Brave, &base_url);
    let results = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Brave Search");
    assert_eq!(results[0].url, "https://search.brave.com");
    assert_eq!(results[0].snippet.as_deref(), Some("Brave Search engine"));
    assert_eq!(results[0].source, "brave_test");
    assert_eq!(results[1].title, "Brave Browser");
}

#[tokio::test]
async fn brave_search_decodes_gzip_json_response() {
    let body = gzip_json(&brave_results_json());
    let router = axum::Router::new().route(
        "/res/v1/web/search",
        axum::routing::get(move || {
            let body = body.clone();
            async move { gzip_response(body, "application/json") }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Brave, &base_url);
    let results = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Brave Search");
}

#[tokio::test]
async fn search_response_limit_applies_after_gzip_decode() {
    let body = gzip_text(&"x".repeat(SEARCH_RESPONSE_BYTES + 1));
    let router = axum::Router::new().route(
        "/search",
        axum::routing::get(move || {
            let body = body.clone();
            async move { gzip_response(body, "text/plain") }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let response = Client::builder()
        .timeout(timeout(&test_fetch_config()))
        .build()
        .unwrap()
        .get(format!("http://{addr}/search"))
        .send()
        .await
        .unwrap();
    let error = read_search_response(response, "gzip_test")
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&error);
    assert_eq!(tool_error.kind, "response_too_large");
}

#[tokio::test]
async fn deepseek_search_request_mapping_and_limit() {
    let router = axum::Router::new().route(
        "/v1/tools/search",
        axum::routing::post(
            |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer test-key-123");
                assert_eq!(headers["content-type"], "application/json");
                assert_eq!(
                    body,
                    json!({"query": "搜索🦀", "count": 2, "need_content": false})
                );
                axum::Json(json!({
                    "results": [
                        {"title": "Unsafe", "url": "javascript:alert(1)"},
                        {
                            "title": " First ",
                            "url": "https://user:secret@example.com/one#section",
                            "summary": " Summary ",
                            "snippet": "not preferred",
                            "content": "not exposed",
                            "date": "2026-10-18",
                            "unmapped_field": "not exposed"
                        },
                        {
                            "title": "Second",
                            "url": "https://example.com/two",
                            "summary": " \n ",
                            "snippet": " Fallback "
                        },
                        {"title": "Third", "url": "https://example.com/three"}
                    ]
                }))
            },
        ),
    );
    let base_url = deepseek_mock_server(router).await;
    let provider = test_provider(WebProviderKind::DeepSeek, &base_url);
    let results = deepseek_search(
        "搜索🦀",
        2,
        "deepseek_test",
        &provider,
        &test_fetch_config(),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 2);
    let first = &results[0];
    assert_eq!(first.title, "First");
    assert_eq!(first.url, "https://example.com/one#section");
    assert_eq!(first.snippet.as_deref(), Some("Summary"));
    assert_eq!(first.published_at.as_deref(), Some("2026-10-18"));
    assert_eq!(
        first.canonical_url.as_deref(),
        Some("https://example.com/one")
    );
    assert_eq!(first.display_url.as_deref(), Some("example.com/one"));
    assert_eq!(first.source, "deepseek_test");
    assert_eq!(first.rank, Some(2));
    assert!(first.metadata.is_none());
    assert!(first.language.is_none());
    assert!(first.region.is_none());
    assert_eq!(results[1].snippet.as_deref(), Some("Fallback"));
}

async fn deepseek_mock_server(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn deepseek_search_count_is_capped() {
    let router = axum::Router::new().route(
        "/v1/tools/search",
        axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            assert_eq!(body["count"], 50);
            axum::Json(json!({
                "results": (0..55)
                    .map(|i| json!({
                        "title": format!("Result {i}"),
                        "url": format!("https://example.com/{i}"),
                        "snippet": "snippet without summary"
                    }))
                    .collect::<Vec<_>>()
            }))
        }),
    );
    let base_url = deepseek_mock_server(router).await;
    let provider = test_provider(WebProviderKind::DeepSeek, &base_url);
    let results = deepseek_search("test", 99, "deepseek_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 50);
    assert_eq!(
        results[0].snippet.as_deref(),
        Some("snippet without summary")
    );
}

#[tokio::test]
async fn deepseek_search_empty_results_succeeds() {
    let router = axum::Router::new().route(
        "/v1/tools/search",
        axum::routing::post(|| async { axum::Json(empty_results_json()) }),
    );
    let base_url = deepseek_mock_server(router).await;
    let config = test_search_config(
        vec![(
            "deepseek_test",
            test_provider(WebProviderKind::DeepSeek, &base_url),
        )],
        vec!["deepseek_test"],
        WebSearchMode::Single,
    );
    let response = search(
        WebSearchRequest {
            query: "no matches".to_string(),
            max_results: Some(5),
            provider: Some("deepseek_test".to_string()),
        },
        &config,
    )
    .await
    .unwrap();
    assert!(response.results.is_empty());
    assert!(response.citations.is_empty());
    assert_eq!(response.provider, "deepseek_test");
    assert_eq!(
        response.provider_attempts[0].status,
        WebSearchProviderAttemptStatus::Success
    );
}

#[tokio::test]
async fn deepseek_search_http_errors_are_redacted() {
    for (status, expected_kind) in [
        (StatusCode::UNAUTHORIZED, "provider_unavailable"),
        (StatusCode::FORBIDDEN, "provider_unavailable"),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        (StatusCode::INTERNAL_SERVER_ERROR, "network_failed"),
        (StatusCode::FOUND, "network_failed"),
    ] {
        let router = axum::Router::new().route(
            "/v1/tools/search",
            axum::routing::post(move || async move {
                (status, "private response: test-key-123 response-secret")
            }),
        );
        let base_url = deepseek_mock_server(router).await;
        let provider = test_provider(WebProviderKind::DeepSeek, &base_url);
        let error = deepseek_search("test", 5, "deepseek_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert_eq!(crate::test_tool_error(&error).kind, expected_kind);
        let diagnostic = format!("{error:?}");
        assert!(!diagnostic.contains("test-key-123"));
        assert!(!diagnostic.contains("response-secret"));
    }
}

#[tokio::test]
async fn deepseek_search_invalid_payloads_are_redacted() {
    for body in [
        "not JSON response-secret test-key-123",
        r#"{"response-secret":"test-key-123"}"#,
        r#"{"results":{}}"#,
        r#"{"results":[{"title":"invalid","url":"file:///private"}]}"#,
    ] {
        let router = axum::Router::new().route(
            "/v1/tools/search",
            axum::routing::post(move || async move { body }),
        );
        let base_url = deepseek_mock_server(router).await;
        let provider = test_provider(WebProviderKind::DeepSeek, &base_url);
        let error = deepseek_search("test", 5, "deepseek_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert_eq!(crate::test_tool_error(&error).kind, "parse_failed");
        let diagnostic = format!("{error:?}");
        assert!(!diagnostic.contains("response-secret"));
        assert!(!diagnostic.contains("test-key-123"));
    }
}

#[tokio::test]
async fn deepseek_search_missing_api_key_is_error() {
    let mut provider = test_provider(WebProviderKind::DeepSeek, "http://127.0.0.1:1");
    provider.api_key.clear();
    let error = deepseek_search("test", 5, "deepseek_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert_eq!(crate::test_tool_error(&error).kind, "provider_unavailable");
    assert!(error.to_string().contains("requires an API key"));
}

#[tokio::test]
async fn deepseek_search_unicode_query_boundaries() {
    let router = axum::Router::new().route(
        "/v1/tools/search",
        axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            let query = body["query"].as_str().unwrap();
            assert!((1..=128).contains(&query.chars().count()));
            assert_eq!(body["count"], 1);
            axum::Json(empty_results_json())
        }),
    );
    let base_url = deepseek_mock_server(router).await;
    let provider = test_provider(WebProviderKind::DeepSeek, &base_url);
    for query in ["🦀".to_string(), "🦀".repeat(128), "a".repeat(128)] {
        assert!(
            deepseek_search(&query, 1, "deepseek_test", &provider, &test_fetch_config(),)
                .await
                .unwrap()
                .is_empty()
        );
    }
    for query in [
        String::new(),
        " \t\n".into(),
        "🦀".repeat(129),
        "a".repeat(129),
    ] {
        let error = deepseek_search(&query, 1, "deepseek_test", &provider, &test_fetch_config())
            .await
            .unwrap_err();
        assert_eq!(crate::test_tool_error(&error).kind, "invalid_request");
    }
}

#[tokio::test]
async fn deepseek_search_invalid_base_url_is_redacted() {
    let provider = test_provider(WebProviderKind::DeepSeek, "invalid secret-base-url");
    let error = deepseek_search("test", 5, "deepseek_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert_eq!(crate::test_tool_error(&error).kind, "provider_unavailable");
    assert!(!format!("{error:?}").contains("secret-base-url"));
}

#[tokio::test]
async fn deepseek_search_response_byte_limit() {
    let router = axum::Router::new().route(
        "/v1/tools/search",
        axum::routing::post(|| async { "x".repeat(SEARCH_RESPONSE_BYTES + 1) }),
    );
    let base_url = deepseek_mock_server(router).await;
    let provider = test_provider(WebProviderKind::DeepSeek, &base_url);
    let error = deepseek_search("test", 5, "deepseek_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert_eq!(crate::test_tool_error(&error).kind, "response_too_large");
}

#[tokio::test]
async fn tavily_search_integration_success() {
    let router = axum::Router::new().route(
        "/search",
        axum::routing::post(|| async { axum::Json(tavily_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Tavily, &base_url);
    let results = tavily_search("test", 5, "tavily_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Tavily Search");
    assert_eq!(results[1].title, "Tavily Docs");
}

#[tokio::test]
async fn keenable_search_integration_success() {
    let router = axum::Router::new().route(
        "/v1/search",
        axum::routing::post(|headers: axum::http::HeaderMap| async move {
            assert_eq!(headers["x-api-key"], "test-key-123");
            axum::Json(serde_json::json!({
                "results": [
                    {
                        "title": "Keenable",
                        "url": "https://keenable.ai",
                        "description": "",
                        "snippet": "Web search for agents",
                        "published_at": "2026-01-01T00:00:00Z"
                    },
                    { "title": "No URL" }
                ]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Keenable, &base_url);
    let results = keenable_search("test", 5, "keenable_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Keenable");
}

#[test]
fn keenable_request_is_key_free_without_api_key() {
    let client = Client::new();
    let request = keenable_request(&client, "https://api.keenable.ai/", " ", "q", 50)
        .build()
        .unwrap();
    assert_eq!(
        request.url().as_str(),
        "https://api.keenable.ai/v1/search/public"
    );
    assert_eq!(request.headers()["X-Keenable-Title"], KEENABLE_APP_TITLE);
    assert!(request.headers().get("X-API-Key").is_none());

    let request = keenable_request(&client, "https://api.keenable.ai", "k", "q", 5)
        .build()
        .unwrap();
    assert_eq!(request.url().as_str(), "https://api.keenable.ai/v1/search");
    assert_eq!(request.headers()["X-API-Key"], "k");
}

#[tokio::test]
async fn exa_search_integration_success() {
    let router = axum::Router::new().route(
        "/search",
        axum::routing::post(|| async { axum::Json(exa_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Exa, &base_url);
    let results = exa_search("test", 5, "exa_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Exa Search");
    assert_eq!(results[0].url, "https://exa.ai");
    assert_eq!(
        results[0].snippet.as_deref(),
        Some("Semantic search engine")
    );
    assert_eq!(results[1].snippet.as_deref(), Some("Exa API documentation"));
}

#[tokio::test]
async fn perplexity_search_integration_success() {
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|| async { axum::Json(perplexity_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Perplexity, &base_url);
    let results = perplexity_search(
        "test",
        5,
        "perplexity_test",
        &provider,
        &test_fetch_config(),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Perplexity Search");
    assert_eq!(results[0].url, "https://www.perplexity.ai");
    assert_eq!(
        results[0].snippet.as_deref(),
        Some("Perplexity summarized these search results.")
    );
    assert_eq!(results[0].published_at.as_deref(), Some("2026-05-16"));
}

#[tokio::test]
async fn firecrawl_search_integration_success() {
    let router = axum::Router::new().route(
        "/v1/search",
        axum::routing::post(|| async { axum::Json(firecrawl_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
    let results = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Firecrawl Search");
    assert_eq!(
        results[1].snippet.as_deref(),
        Some("Firecrawl API documentation")
    );
}

#[tokio::test]
async fn perplexity_search_empty_results_is_error() {
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|| async { axum::Json(empty_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Perplexity, &base_url);
    let err = perplexity_search(
        "test",
        5,
        "perplexity_test",
        &provider,
        &test_fetch_config(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("no parseable search results"));
}

#[tokio::test]
async fn perplexity_search_invalid_json_is_error() {
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|| async { "not json" }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Perplexity, &base_url);
    let err = perplexity_search(
        "test",
        5,
        "perplexity_test",
        &provider,
        &test_fetch_config(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("invalid JSON"));
}

#[tokio::test]
async fn perplexity_search_http_401_is_error() {
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Perplexity, &base_url);
    let err = perplexity_search(
        "test",
        5,
        "perplexity_test",
        &provider,
        &test_fetch_config(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("HTTP 401"));
}

#[tokio::test]
async fn perplexity_search_http_429_is_error() {
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Perplexity, &base_url);
    let err = perplexity_search(
        "test",
        5,
        "perplexity_test",
        &provider,
        &test_fetch_config(),
    )
    .await
    .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "rate_limited");
}

#[tokio::test]
async fn perplexity_search_missing_api_key_is_error() {
    let provider = WebProviderConfig {
        kind: WebProviderKind::Perplexity,
        base_url: Some("http://localhost:1".to_string()),
        api_key: String::new(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    };

    let err = perplexity_search(
        "test",
        5,
        "perplexity_test",
        &provider,
        &test_fetch_config(),
    )
    .await
    .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "provider_unavailable");
    assert!(err.to_string().contains("API key"));
}

#[tokio::test]
async fn firecrawl_search_empty_results_is_error() {
    let router = axum::Router::new().route(
        "/v1/search",
        axum::routing::post(|| async { axum::Json(empty_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
    let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no parseable search results"));
}

#[tokio::test]
async fn firecrawl_search_invalid_json_is_error() {
    let router =
        axum::Router::new().route("/v1/search", axum::routing::post(|| async { "not json" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
    let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("invalid JSON"));
}

#[tokio::test]
async fn firecrawl_search_http_401_is_error() {
    let router = axum::Router::new().route(
        "/v1/search",
        axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
    let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 401"));
}

#[tokio::test]
async fn firecrawl_search_http_429_is_error() {
    let router = axum::Router::new().route(
        "/v1/search",
        axum::routing::post(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Firecrawl, &base_url);
    let err = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "rate_limited");
}

#[test]
fn firecrawl_request_is_keyless_without_api_key() {
    let client = Client::new();
    let request = firecrawl_request(&client, "https://api.firecrawl.dev/", " ", "q", 50)
        .build()
        .unwrap();
    assert_eq!(
        request.url().as_str(),
        "https://api.firecrawl.dev/v1/search"
    );
    assert!(request.headers().get("Authorization").is_none());

    let request = firecrawl_request(&client, "https://api.firecrawl.dev", "k", "q", 5)
        .build()
        .unwrap();
    assert_eq!(
        request.url().as_str(),
        "https://api.firecrawl.dev/v1/search"
    );
    assert_eq!(request.headers()["Authorization"], "Bearer k");
}

#[tokio::test]
async fn firecrawl_search_keyless_without_api_key_uses_free_tier() {
    let router = axum::Router::new().route(
        "/v1/search",
        axum::routing::post(|| async { axum::Json(firecrawl_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = WebProviderConfig {
        kind: WebProviderKind::Firecrawl,
        base_url: Some(base_url),
        api_key: String::new(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    };
    let results = firecrawl_search("test", 5, "firecrawl_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn brave_search_empty_results_is_error() {
    let router = axum::Router::new().route(
        "/res/v1/web/search",
        axum::routing::get(|| async { axum::Json(empty_brave_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Brave, &base_url);
    let err = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("no parseable search results"),
        "expected empty results error, got: {err}"
    );
}

#[tokio::test]
async fn brave_search_http_401_is_error() {
    let router = axum::Router::new().route(
        "/res/v1/web/search",
        axum::routing::get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Brave, &base_url);
    let err = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("HTTP 401"),
        "expected HTTP 401 error, got: {err}"
    );
}

#[tokio::test]
async fn brave_search_http_429_is_error() {
    let router = axum::Router::new().route(
        "/res/v1/web/search",
        axum::routing::get(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Brave, &base_url);
    let err = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "rate_limited");
}

#[tokio::test]
async fn brave_search_missing_api_key_is_error() {
    let provider = WebProviderConfig {
        kind: WebProviderKind::Brave,
        base_url: Some("http://localhost:1".to_string()),
        api_key: String::new(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    };
    let err = brave_search("test", 5, "brave_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "provider_unavailable");
    assert!(
        format!("{err}").contains("API key"),
        "expected API key error, got: {err}"
    );
}

#[tokio::test]
async fn tencent_cloud_wsa_search_integration_success() {
    let router = axum::Router::new().route(
        "/SearchPro",
        axum::routing::post(|| async { axum::Json(tencent_cloud_wsa_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::TencentCloudWsa, &base_url);
    let results =
        tencent_cloud_wsa_search("test", 5, "tencent_test", &provider, &test_fetch_config())
            .await
            .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Tencent Cloud Search");
    assert_eq!(results[0].url, "https://cloud.tencent.com/product/wsa");
    assert_eq!(
        results[0].snippet.as_deref(),
        Some("Tencent Cloud WSA SearchPro")
    );
    assert_eq!(results[0].source, "tencent_test");
    assert_eq!(results[0].published_at.as_deref(), Some("2026-07-01"));
    assert_eq!(
        results[0]
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("site")),
        Some(&json!("cloud.tencent.com"))
    );
    assert_eq!(results[1].title, "SearchPro Docs");
}

#[tokio::test]
async fn tencent_cloud_wsa_search_empty_results_is_error() {
    let router = axum::Router::new().route(
        "/SearchPro",
        axum::routing::post(|| async { axum::Json(empty_tencent_cloud_wsa_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::TencentCloudWsa, &base_url);
    let err = tencent_cloud_wsa_search("test", 5, "tencent_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("no parseable search results"),
        "expected empty results error, got: {err}"
    );
}

#[tokio::test]
async fn tencent_cloud_wsa_search_http_401_is_error() {
    let router = axum::Router::new().route(
        "/SearchPro",
        axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::TencentCloudWsa, &base_url);
    let err = tencent_cloud_wsa_search("test", 5, "tencent_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("HTTP 401"),
        "expected HTTP 401 error, got: {err}"
    );
}

#[tokio::test]
async fn tencent_cloud_wsa_search_http_429_is_error() {
    let router = axum::Router::new().route(
        "/SearchPro",
        axum::routing::post(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::TencentCloudWsa, &base_url);
    let err = tencent_cloud_wsa_search("test", 5, "tencent_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "rate_limited");
}

#[tokio::test]
async fn tencent_cloud_wsa_search_missing_api_key_is_error() {
    let provider = WebProviderConfig {
        kind: WebProviderKind::TencentCloudWsa,
        base_url: Some("http://localhost:1".to_string()),
        api_key: String::new(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    };
    let err = tencent_cloud_wsa_search("test", 5, "tencent_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "provider_unavailable");
    assert!(
        format!("{err}").contains("API key"),
        "expected API key error, got: {err}"
    );
}

#[tokio::test]
async fn bocha_search_integration_success() {
    let router = axum::Router::new().route(
        "/v1/web-search",
        axum::routing::post(|| async { axum::Json(bocha_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Bocha, &base_url);
    let results = bocha_search("test", 5, "bocha_test", &provider, &test_fetch_config())
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Bocha Search");
    assert_eq!(results[0].url, "https://bochaai.com");
    assert_eq!(results[0].snippet.as_deref(), Some("AI web search API"));
    assert_eq!(results[0].published_at.as_deref(), Some("2026-07-02"));
    assert_eq!(results[0].source, "bocha_test");
    assert_eq!(results[1].language.as_deref(), Some("zh"));
}

#[tokio::test]
async fn bocha_search_empty_results_is_error() {
    let router = axum::Router::new().route(
        "/v1/web-search",
        axum::routing::post(|| async { axum::Json(empty_bocha_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Bocha, &base_url);
    let err = bocha_search("test", 5, "bocha_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("no parseable search results"),
        "expected empty results error, got: {err}"
    );
}

#[tokio::test]
async fn bocha_search_http_401_is_error() {
    let router = axum::Router::new().route(
        "/v1/web-search",
        axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Bocha, &base_url);
    let err = bocha_search("test", 5, "bocha_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("HTTP 401"),
        "expected HTTP 401 error, got: {err}"
    );
}

#[tokio::test]
async fn bocha_search_http_429_is_error() {
    let router = axum::Router::new().route(
        "/v1/web-search",
        axum::routing::post(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Bocha, &base_url);
    let err = bocha_search("test", 5, "bocha_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "rate_limited");
}

#[tokio::test]
async fn bocha_search_missing_api_key_is_error() {
    let provider = WebProviderConfig {
        kind: WebProviderKind::Bocha,
        base_url: Some("http://localhost:1".to_string()),
        api_key: String::new(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    };
    let err = bocha_search("test", 5, "bocha_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    let tool_error = crate::test_tool_error(&err);
    assert_eq!(tool_error.kind, "provider_unavailable");
    assert!(
        format!("{err}").contains("API key"),
        "expected API key error, got: {err}"
    );
}

#[tokio::test]
async fn tavily_search_invalid_json_is_error() {
    let router = axum::Router::new().route("/search", axum::routing::post(|| async { "not json" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Tavily, &base_url);
    let err = tavily_search("test", 5, "tavily_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("invalid JSON"),
        "expected invalid JSON error, got: {err}"
    );
}

#[tokio::test]
async fn exa_search_empty_results_is_error() {
    let router = axum::Router::new().route(
        "/search",
        axum::routing::post(|| async { axum::Json(empty_results_json()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base_url = format!("http://{}", addr);

    let provider = test_provider(WebProviderKind::Exa, &base_url);
    let err = exa_search("test", 5, "exa_test", &provider, &test_fetch_config())
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("no parseable search results"),
        "expected empty results error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Real API integration tests (opt-in, requires API keys)
// ---------------------------------------------------------------------------

/// Real Brave Search API integration test.
/// Set BRAVE_API_KEY env var to run: BRAVE_API_KEY=... cargo test brave_search_live -- --ignored
#[tokio::test]
#[ignore = "requires BRAVE_API_KEY env var and network access"]
async fn brave_search_live_integration() {
    let api_key = std::env::var("BRAVE_API_KEY").ok();
    if api_key.is_none() {
        eprintln!("SKIP: BRAVE_API_KEY not set");
        return;
    }
    let api_key = api_key.unwrap();
    assert!(!api_key.is_empty(), "BRAVE_API_KEY is empty");

    let provider = WebProviderConfig {
        kind: WebProviderKind::Brave,
        base_url: None, // use default https://api.search.brave.com
        api_key,
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    };
    let fetch_config = test_fetch_config();

    let results = brave_search(
        "Rust programming language",
        3,
        "brave_live",
        &provider,
        &fetch_config,
    )
    .await
    .expect("Brave live search should succeed");

    eprintln!("Brave live search returned {} results", results.len());
    for (i, r) in results.iter().enumerate() {
        eprintln!(
            "  [{i}] title={} url={} snippet={:?}",
            r.title, r.url, r.snippet
        );
    }
    assert!(
        !results.is_empty(),
        "Brave live search should return at least 1 result"
    );
    assert!(
        !results[0].title.is_empty(),
        "first result should have a title"
    );
    assert!(!results[0].url.is_empty(), "first result should have a url");
}

fn gzip_json(value: &serde_json::Value) -> Vec<u8> {
    gzip_text(&value.to_string())
}

fn gzip_text(text: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

fn gzip_response(body: Vec<u8>, content_type: &'static str) -> Response {
    let mut response = Response::new(Body::from(body));
    response
        .headers_mut()
        .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

// ---------------------------------------------------------------------------
// Native search providers (explicit-only dispatch boundary)
// ---------------------------------------------------------------------------

fn native_provider_config(model_route: Option<&str>) -> WebProviderConfig {
    WebProviderConfig {
        kind: WebProviderKind::OpenAiNative,
        base_url: None,
        api_key: String::new(),
        model_route: model_route.map(str::to_string),
        command: None,
        output: None,
        limits: Default::default(),
    }
}

struct RecordingNativeDispatcher {
    calls: Mutex<Vec<String>>,
    outcome: std::result::Result<Vec<WebSearchResult>, WebSearchNativeDispatchError>,
}

impl RecordingNativeDispatcher {
    fn ok(results: Vec<WebSearchResult>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcome: Ok(results),
        }
    }

    fn err(kind: &'static str, message: &str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcome: Err(WebSearchNativeDispatchError {
                kind,
                message: message.to_string(),
                recovery_hint: None,
            }),
        }
    }

    fn dispatched(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl WebSearchNativeDispatcher for RecordingNativeDispatcher {
    async fn dispatch(
        &self,
        provider_id: &str,
        model_route: &str,
        query: &str,
        max_results: usize,
    ) -> std::result::Result<Vec<WebSearchResult>, WebSearchNativeDispatchError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{provider_id}|{model_route}|{query}|{max_results}"));
        self.outcome.clone()
    }
}

fn native_search_result(url: &str) -> WebSearchResult {
    WebSearchResult {
        title: format!("Native result {url}"),
        url: url.to_string(),
        snippet: None,
        source: "openai-search".to_string(),
        published_at: None,
        canonical_url: None,
        display_url: None,
        rank: None,
        language: None,
        region: None,
        metadata: None,
    }
}

#[tokio::test]
async fn native_default_provider_requires_explicit_selection() {
    let mut config = test_search_config(
        vec![(
            "openai-search",
            native_provider_config(Some("openai@default/gpt-test")),
        )],
        vec![],
        WebSearchMode::Fallback,
    );
    config.search.provider = "openai-search".to_string();
    let error = super::search_with_native(test_tool_error, routing_request(None), &config, None)
        .await
        .unwrap_err();
    let tool_error = test_tool_error(&error);
    assert_eq!(tool_error.kind, "provider_unavailable");
    assert!(tool_error.message.contains("explicit provider argument"));
}

#[tokio::test]
async fn native_provider_without_model_route_never_dispatches() {
    let config = test_search_config(
        vec![("openai-search", native_provider_config(None))],
        vec![],
        WebSearchMode::Fallback,
    );
    let dispatcher =
        RecordingNativeDispatcher::ok(vec![native_search_result("https://example.test/native")]);
    let error = super::search_with_native(
        test_tool_error,
        routing_request(Some("openai-search")),
        &config,
        Some(&dispatcher),
    )
    .await
    .unwrap_err();
    let tool_error = test_tool_error(&error);
    assert_eq!(tool_error.kind, "provider_unavailable");
    assert!(tool_error.message.contains("no model_route bound"));
    assert!(dispatcher.dispatched().is_empty());
}

#[tokio::test]
async fn native_provider_without_attached_runtime_fails() {
    let config = test_search_config(
        vec![(
            "openai-search",
            native_provider_config(Some("openai@default/gpt-test")),
        )],
        vec![],
        WebSearchMode::Fallback,
    );
    let error = super::search_with_native(
        test_tool_error,
        routing_request(Some("openai-search")),
        &config,
        None,
    )
    .await
    .unwrap_err();
    let tool_error = test_tool_error(&error);
    assert_eq!(tool_error.kind, "provider_unavailable");
    assert!(tool_error.message.contains("no native search runtime"));
}

#[tokio::test]
async fn explicit_native_provider_dispatches_and_maps_results() {
    let config = test_search_config(
        vec![(
            "openai-search",
            native_provider_config(Some("openai@default/gpt-test")),
        )],
        vec![],
        WebSearchMode::Fallback,
    );
    let dispatcher = RecordingNativeDispatcher::ok(vec![
        native_search_result("https://example.test/first"),
        native_search_result("https://example.test/second"),
    ]);
    let response = super::search_with_native(
        test_tool_error,
        routing_request(Some("openai-search")),
        &config,
        Some(&dispatcher),
    )
    .await
    .unwrap();
    assert_eq!(response.provider, "openai-search");
    assert_eq!(response.results.len(), 2);
    assert_eq!(response.results[0].url, "https://example.test/first");
    assert_eq!(response.provider_attempts.len(), 1);
    assert_eq!(
        response.provider_attempts[0].status,
        WebSearchProviderAttemptStatus::Success
    );
    assert_eq!(response.winning_provider.as_deref(), Some("openai-search"));
    assert_eq!(
        dispatcher.dispatched(),
        vec!["openai-search|openai@default/gpt-test|fixture query|2".to_string()]
    );
}

#[tokio::test]
async fn native_search_not_executed_fails_without_fallback() {
    let config = test_search_config(
        vec![(
            "openai-search",
            native_provider_config(Some("openai@default/gpt-test")),
        )],
        vec![],
        WebSearchMode::Fallback,
    );
    let dispatcher = RecordingNativeDispatcher::err(
        "search_not_executed",
        "upstream returned no search evidence",
    );
    let error = super::search_with_native(
        test_tool_error,
        routing_request(Some("openai-search")),
        &config,
        Some(&dispatcher),
    )
    .await
    .unwrap_err();
    let tool_error = test_tool_error(&error);
    assert_eq!(tool_error.kind, "search_not_executed");
    assert_eq!(dispatcher.dispatched().len(), 1);
}

#[test]
fn provider_order_auto_excludes_native_providers() {
    let config = test_search_config(
        vec![
            (
                "openai-search",
                native_provider_config(Some("openai@default/gpt-test")),
            ),
            (
                "duck",
                test_provider(WebProviderKind::DuckDuckGo, "https://example.test"),
            ),
        ],
        vec!["openai-search", "duck"],
        WebSearchMode::Fallback,
    );
    assert_eq!(provider_order("auto", &config), vec!["duck".to_string()]);
}

#[tokio::test]
async fn auto_mode_never_dispatches_native_providers() {
    let base_url = searxng_mock_base_url(searxng_results_json(&[(
        "Managed result",
        "https://example.test/managed",
        "ok",
    )]))
    .await;
    let config = test_search_config(
        vec![
            (
                "openai-search",
                native_provider_config(Some("openai@default/gpt-test")),
            ),
            (
                "managed",
                test_provider(WebProviderKind::Searxng, &base_url),
            ),
        ],
        vec!["openai-search", "managed"],
        WebSearchMode::Fallback,
    );
    let dispatcher =
        RecordingNativeDispatcher::ok(vec![native_search_result("https://example.test/native")]);
    let response = super::search_with_native(
        test_tool_error,
        routing_request(Some("auto")),
        &config,
        Some(&dispatcher),
    )
    .await
    .unwrap();
    assert!(dispatcher.dispatched().is_empty());
    assert_eq!(response.winning_provider.as_deref(), Some("managed"));
}
