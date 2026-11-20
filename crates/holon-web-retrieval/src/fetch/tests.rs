use super::*;
use axum::{
    body::Body,
    extract::Request,
    http::{
        header::{
            ACCEPT as AXUM_ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE as AXUM_ACCEPT_LANGUAGE,
            CONTENT_ENCODING, CONTENT_TYPE, USER_AGENT as AXUM_USER_AGENT,
        },
        HeaderValue,
    },
    response::Response,
};
use flate2::{write::GzEncoder, Compression};
use std::io::Write;

#[test]
fn html_extraction_removes_tags() {
    let text = strip_html_tags("<html><body><h1>Hello</h1><p>A &amp; B</p></body></html>");
    assert!(text.contains("Hello"));
    assert!(text.contains("A & B"));
    assert!(!text.contains("<h1>"));
}

#[test]
fn wrapper_marks_external_content_as_untrusted() {
    let url = Url::parse("https://example.com/").unwrap();
    let wrapped = external_content_wrapper(&url, "hello");
    assert!(wrapped.contains("external_content"));
    assert!(wrapped.contains("untrusted"));
}

#[tokio::test]
async fn fetch_sends_browser_request_headers() {
    let router = axum::Router::new().route(
        "/headers",
        axum::routing::get(|request: Request| async move {
            let headers = request.headers();
            assert_eq!(headers.get(AXUM_USER_AGENT).unwrap(), WEB_FETCH_USER_AGENT);
            assert_eq!(headers.get(AXUM_ACCEPT).unwrap(), WEB_FETCH_ACCEPT);
            assert_eq!(
                headers.get(AXUM_ACCEPT_LANGUAGE).unwrap(),
                WEB_FETCH_ACCEPT_LANGUAGE
            );
            assert_eq!(headers.get(ACCEPT_ENCODING).unwrap(), "gzip");
            "headers received"
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let mut config = WebFetchConfig::default();
    config.allowed_hosts = vec![format!("127.0.0.1:{}", addr.port())];
    let response = fetch(
        WebFetchRequest {
            url: format!("http://{addr}/headers"),
            max_chars: None,
            extract_mode: ExtractMode::Auto,
        },
        &config,
    )
    .await
    .unwrap();

    // The local route carries no provider attribution and reports the
    // target page's own HTTP status.
    assert_eq!(response.provider_id, None);
    assert_eq!(response.status_source, "target_http");
    assert!(response.text.contains("headers received"));
}

#[test]
fn list_fetch_providers_mirrors_route_gating() {
    let mut config = WebConfig::default();
    let listed = list_fetch_providers(&config);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].provider_id, None);
    assert_eq!(listed[0].kind, "local_http");
    assert_eq!(listed[0].extract_modes, vec!["auto", "text", "raw"]);

    let mut provider = WebProviderConfig {
        kind: WebProviderKind::DeepSeek,
        base_url: None,
        api_key: "fixture-key".into(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    };
    config
        .providers
        .insert("deepseek-a".into(), provider.clone());
    // WebFetch resolves named providers by registry lookup with no selector
    // semantics, so reserved words and padded IDs still name routes here.
    config.providers.insert("auto".into(), provider.clone());
    config
        .providers
        .insert(" deepseek-b".into(), provider.clone());
    // Tavily is callable remotely with both documented extract formats.
    let mut tavily = provider.clone();
    tavily.kind = WebProviderKind::Tavily;
    config.providers.insert("tavily-a".into(), tavily);
    // Exa is callable only in auto mode.
    let mut exa = provider.clone();
    exa.kind = WebProviderKind::Exa;
    config.providers.insert("exa-a".into(), exa.clone());
    let mut malformed = exa.clone();
    malformed.base_url = Some("https://exa.example/api?secret=fixture".into());
    config.providers.insert("exa-bad-url".into(), malformed);
    let mut invalid_key = exa.clone();
    invalid_key.api_key = "fixture\nkey".into();
    config
        .providers
        .insert("exa-invalid-key".into(), invalid_key);
    exa.api_key.clear();
    config.providers.insert("exa-no-key".into(), exa);
    // Firecrawl Scrape is callable for auto extraction only and requires a
    // key even though Firecrawl Search works keyless.
    let mut firecrawl = provider.clone();
    firecrawl.kind = WebProviderKind::Firecrawl;
    let mut firecrawl_no_key = firecrawl.clone();
    firecrawl_no_key.api_key.clear();
    config.providers.insert("firecrawl-a".into(), firecrawl);
    config
        .providers
        .insert("firecrawl-no-key".into(), firecrawl_no_key);
    // Search-only kinds and missing credentials stay undiscoverable.
    let mut search_only = provider.clone();
    search_only.kind = WebProviderKind::Brave;
    config.providers.insert("brave-search".into(), search_only);
    provider.api_key.clear();
    config.providers.insert("no-key".into(), provider);

    let listed = list_fetch_providers(&config);
    let remote_ids: Vec<Option<&str>> = listed[1..]
        .iter()
        .map(|info| info.provider_id.as_deref())
        .collect();
    assert_eq!(
        remote_ids,
        vec![
            Some(" deepseek-b"),
            Some("auto"),
            Some("deepseek-a"),
            Some("exa-a"),
            Some("firecrawl-a"),
            Some("tavily-a")
        ]
    );
    assert_eq!(listed[3].kind, "deepseek");
    assert_eq!(listed[3].extract_modes, vec!["auto"]);
    assert_eq!(listed[4].kind, "exa");
    assert_eq!(listed[4].extract_modes, vec!["auto"]);
    assert_eq!(listed[5].kind, "firecrawl");
    assert_eq!(listed[5].extract_modes, vec!["auto"]);
    assert_eq!(listed[6].kind, "tavily");
    assert_eq!(listed[6].extract_modes, vec!["auto", "text"]);

    // The shared preflight rejects every remote request once custom host
    // rules are configured, so discovery must not offer those routes.
    config.fetch.allowed_hosts = vec!["example.com".into()];
    let listed = list_fetch_providers(&config);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].provider_id, None);

    config.fetch.enabled = false;
    assert!(list_fetch_providers(&config).is_empty());
}

#[test]
fn list_fetch_providers_hides_malformed_tavily_base_url() {
    let mut config = WebConfig::default();
    let provider = WebProviderConfig {
        kind: WebProviderKind::Tavily,
        base_url: Some("not a url".into()),
        api_key: "fixture-key".into(),
        model_route: None,
        command: None,
        output: None,
        limits: Default::default(),
    };
    config
        .providers
        .insert("tavily-bad".into(), provider.clone());
    assert_eq!(list_fetch_providers(&config).len(), 1);

    // The same provider is discoverable once the base URL is a valid endpoint.
    config.providers.insert(
        "tavily-bad".into(),
        WebProviderConfig {
            base_url: Some("https://api.tavily.com".into()),
            ..provider
        },
    );
    let listed = list_fetch_providers(&config);
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].provider_id.as_deref(), Some("tavily-bad"));
    assert_eq!(listed[1].extract_modes, vec!["auto", "text"]);
}

#[tokio::test]
async fn fetch_decodes_gzip_response_before_text_extraction() {
    let body = gzip_bytes("compressed fetch body");
    let router = axum::Router::new().route(
        "/page",
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

    let mut config = WebFetchConfig::default();
    config.allowed_hosts = vec![format!("127.0.0.1:{}", addr.port())];
    let response = fetch(
        WebFetchRequest {
            url: format!("http://{addr}/page"),
            max_chars: None,
            extract_mode: ExtractMode::Auto,
        },
        &config,
    )
    .await
    .unwrap();

    assert!(response.text.contains("compressed fetch body"));
    assert_eq!(response.bytes_read, "compressed fetch body".len());
    assert!(!response.truncated);
}

#[tokio::test]
async fn fetch_revalidates_redirect_targets_before_connecting() {
    let router = axum::Router::new().route(
        "/redirect",
        axum::routing::get(|| async {
            axum::response::Redirect::temporary("http://169.254.169.254/latest/meta-data")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let mut config = WebFetchConfig::default();
    config.allowed_hosts = vec![format!("127.0.0.1:{}", addr.port())];
    let error = fetch(
        WebFetchRequest {
            url: format!("http://{addr}/redirect"),
            max_chars: None,
            extract_mode: ExtractMode::Auto,
        },
        &config,
    )
    .await
    .unwrap_err();

    let tool_error = crate::test_tool_error(&error);
    assert_eq!(tool_error.kind, "network_denied");
}

#[tokio::test]
#[ignore = "requires public network access"]
async fn fetches_akamai_protected_pdf_with_browser_fingerprint() {
    let response = fetch(
        WebFetchRequest {
            url: "https://www.cn.emb-japan.go.jp/files/100706925.pdf".into(),
            max_chars: Some(100),
            extract_mode: ExtractMode::Raw,
        },
        &WebFetchConfig::default(),
    )
    .await
    .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.content_type.as_deref(), Some("application/pdf"));
    assert!(response.bytes_read > 0);
}

#[tokio::test]
#[ignore = "requires public network access"]
async fn fetches_ordinary_https_site_with_browser_fingerprint() {
    let response = fetch(
        WebFetchRequest {
            url: "https://example.com/".into(),
            max_chars: Some(200),
            extract_mode: ExtractMode::Auto,
        },
        &WebFetchConfig::default(),
    )
    .await
    .unwrap();

    assert_eq!(response.status, 200);
    assert!(response.text.contains("Example Domain"));
}

fn gzip_bytes(text: &str) -> Vec<u8> {
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
