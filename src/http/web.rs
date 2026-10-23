use super::*;

pub async fn web_or_not_found_handler(
    State(state): State<Arc<AppState>>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
) -> AxumResponse {
    if matches!(method, Method::GET | Method::HEAD) {
        let head_only = method == Method::HEAD;
        let request_path = uri.path().trim_start_matches('/');
        if normalize_web_asset_path(request_path)
            .is_some_and(|path| matches!(path.split('/').next(), Some("api" | "apps")))
        {
            return not_found("Not Found").into_response();
        }
        if request_path == "login" {
            if let Some(response) =
                web_asset_response(&state, "index.html", &headers, head_only).await
            {
                return response;
            }
            return login_page_response(head_only);
        }
        if !request_path.is_empty() {
            if let Some(response) =
                web_asset_response(&state, request_path, &headers, head_only).await
            {
                return response;
            }
        }
        if accepts_spa_html(request_path, &headers) {
            if state.host.config().auth.mode == crate::authentication::AuthenticationMode::Oidc
                && super::authenticate_session(&headers, &state).is_err()
            {
                return (
                    StatusCode::FOUND,
                    [(LOCATION, HeaderValue::from_static("/login"))],
                )
                    .into_response();
            }
            if request_path.is_empty() && state.host.bootstrap_mode_active() {
                return (
                    StatusCode::FOUND,
                    [(LOCATION, HeaderValue::from_static("/settings"))],
                )
                    .into_response();
            }
            if let Some(response) =
                web_asset_response(&state, "index.html", &headers, head_only).await
            {
                return response;
            }
        }
    }
    not_found("Not Found").into_response()
}

fn login_page_response(head_only: bool) -> AxumResponse {
    let body = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Holon login</title>
  <style>body{font-family:system-ui,sans-serif;max-width:32rem;margin:12vh auto;padding:1.5rem;line-height:1.5}a{display:inline-block;background:#111;color:#fff;padding:.65rem 1rem;border-radius:.5rem;text-decoration:none}.hint{color:#666}</style>
</head>
<body>
  <h1>Sign in to Holon</h1>
  <p class="hint">Continue with the configured Holon authentication method.</p>
  <p><a href="/api/auth/oidc/start">Continue with organization login</a></p>
</body>
</html>"#;
    let body = if head_only {
        Body::empty()
    } else {
        Body::from(body)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .header(CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(crate) fn accepts_html(headers: &HeaderMap) -> bool {
    html_acceptance(headers).is_some_and(|(specificity, quality)| specificity == 2 && quality > 0.0)
}

fn accepts_spa_html(request_path: &str, headers: &HeaderMap) -> bool {
    // Keep the root's explicit-HTML versus JSON behavior separate from SPA navigation.
    if request_path.is_empty() {
        return accepts_html(headers);
    }
    let Some(path) = normalize_web_asset_path(request_path) else {
        return false;
    };
    if matches!(path.split('/').next(), Some("api" | "apps" | "assets")) {
        return false;
    }
    let Some((specificity, quality)) = html_acceptance(headers) else {
        return false;
    };
    if quality <= 0.0 {
        return false;
    }
    // Do not turn missing assets into successful HTML responses. Explicit HTML
    // still supports dotted route identifiers that are not known file types.
    if std::path::Path::new(&path).extension().is_some() {
        return specificity == 2 && mime_guess::from_path(&path).first().is_none();
    }
    true
}

fn html_acceptance(headers: &HeaderMap) -> Option<(u8, f32)> {
    if !headers.contains_key(ACCEPT) {
        return Some((0, 1.0));
    }
    let mut preferred = None;
    for value in headers.get_all(ACCEPT) {
        let accept = value.to_str().ok()?;
        for range in accept.split(',') {
            let mut parts = range.split(';');
            let media_type = parts.next()?.trim();
            let specificity = if media_type.eq_ignore_ascii_case("text/html") {
                2
            } else if media_type.eq_ignore_ascii_case("text/*") {
                1
            } else if media_type == "*/*" {
                0
            } else {
                continue;
            };
            let mut quality = 1.0;
            for parameter in parts {
                if let Some((name, value)) = parameter.trim().split_once('=') {
                    if name.trim().eq_ignore_ascii_case("q") {
                        quality = value
                            .trim()
                            .parse::<f32>()
                            .ok()
                            .filter(|quality| (0.0..=1.0).contains(quality))
                            .unwrap_or(0.0);
                        break;
                    }
                }
            }
            if preferred.is_none_or(|(best_specificity, best_quality)| {
                specificity > best_specificity
                    || (specificity == best_specificity && quality > best_quality)
            }) {
                preferred = Some((specificity, quality));
            }
        }
    }
    preferred
}

pub(crate) async fn web_asset_response(
    state: &AppState,
    request_path: &str,
    headers: &HeaderMap,
    head_only: bool,
) -> Option<AxumResponse> {
    let path = normalize_web_asset_path(request_path)?;
    let bytes = if let Some(web_dist) = &state.web_dist {
        tokio::fs::read(web_dist.join(&path)).await.ok()?
    } else {
        EmbeddedWebAssets::get(&path)?.data.into_owned()
    };
    Some(asset_response(bytes, &path, headers, head_only))
}

/// Cache policy: Vite emits content-hashed files under `assets/`, so those
/// URLs are safe to cache forever; everything else (index.html and other
/// root files) must revalidate so the next full page load picks up a new
/// deployment.
fn web_asset_cache_control(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Build an asset response from already-read bytes: strong content-addressed
/// ETag plus cache policy, with RFC 9110 If-None-Match revalidation (304).
fn asset_response(
    bytes: Vec<u8>,
    path: &str,
    headers: &HeaderMap,
    head_only: bool,
) -> AxumResponse {
    let etag = etag_for_bytes(&bytes);
    let cache_control = web_asset_cache_control(path);
    if if_none_match_satisfied(headers, &etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(ETAG, etag)
            .header(CACHE_CONTROL, cache_control)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let content_type = mime_guess::from_path(path).first_or_octet_stream();
    let body = if head_only {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type.as_ref())
        .header(ETAG, etag)
        .header(CACHE_CONTROL, cache_control)
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(crate) fn normalize_web_asset_path(request_path: &str) -> Option<String> {
    let decoded = percent_decode_str(request_path).decode_utf8().ok()?;
    let decoded = decoded.trim_start_matches('/');
    if decoded.is_empty() || decoded.contains('\\') {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in std::path::Path::new(decoded).components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            _ => return None,
        }
    }
    normalized.to_str().map(|path| path.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::fs;
    use tempfile::tempdir;
    use tower::ServiceExt;

    const TEST_INDEX: &str = "<html>holon spa</html>";

    fn web_router() -> (tempfile::TempDir, Router) {
        let home = tempdir().unwrap();
        fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let config =
            crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host = crate::host::RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new("done")),
        )
        .unwrap();
        let web_dist = home.path().join("web");
        fs::create_dir_all(web_dist.join("assets")).unwrap();
        fs::create_dir_all(web_dist.join("api")).unwrap();
        fs::write(web_dist.join("index.html"), TEST_INDEX).unwrap();
        fs::write(web_dist.join("assets/existing.js"), "export {};").unwrap();
        fs::write(web_dist.join("api/missing"), "not an API response").unwrap();
        let app = crate::http::router(AppState::for_tcp(host).with_web_dist(Some(web_dist)));
        (home, app)
    }

    fn web_request(method: Method, path: &str, accept: Option<&str>) -> AxumRequest<Body> {
        let mut request = AxumRequest::builder().method(method).uri(path);
        if let Some(accept) = accept {
            request = request.header(ACCEPT, accept);
        }
        request.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn spa_navigation_negotiates_html_for_get_and_head() {
        let (_home, app) = web_router();
        for method in [Method::GET, Method::HEAD] {
            for accept in [
                Some("text/html"),
                Some("*/*"),
                None,
                Some("text/*"),
                Some("application/json, text/html;q=0.2"),
                Some("text/html;q=0.5, text/*;q=0, */*;q=0"),
                Some("text/*;q=0.5, */*;q=0"),
                Some("TEXT/HTML; Q=0.5"),
            ] {
                for path in ["/search", "/agents/default/conversation", "/settings/"] {
                    let response = app
                        .clone()
                        .oneshot(web_request(method.clone(), path, accept))
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::OK,
                        "{method} {path} {accept:?}"
                    );
                    assert_eq!(response.headers()[CONTENT_TYPE], "text/html");
                    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                    let expected = if method == Method::HEAD {
                        ""
                    } else {
                        TEST_INDEX
                    };
                    assert_eq!(&body[..], expected.as_bytes());
                }
            }
        }
    }

    #[tokio::test]
    async fn spa_navigation_honors_rejected_html_and_media_range_specificity() {
        let (_home, app) = web_router();
        for method in [Method::GET, Method::HEAD] {
            for accept in [
                "",
                "application/json",
                "text/html;q=0",
                "text/html;q=0, */*;q=1",
                "*/*;q=1, text/html;q=0",
                "text/html;q=0, text/*;q=1",
                "text/*;q=0, */*;q=1",
                "*/*;q=0",
                "text/html;q=invalid, */*",
                "text/html;q=1.5, */*",
                "text/html-other",
            ] {
                let response = app
                    .clone()
                    .oneshot(web_request(method.clone(), "/search", Some(accept)))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{method} {accept}"
                );
            }
        }
        let mut request = web_request(Method::GET, "/search", Some("*/*"));
        request
            .headers_mut()
            .append(ACCEPT, HeaderValue::from_static("text/html;q=0"));
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn spa_fallback_does_not_mask_namespaces_assets_or_non_navigation_methods() {
        let (_home, app) = web_router();
        for method in [Method::GET, Method::HEAD] {
            for accept in [Some("text/html"), Some("*/*"), None] {
                for path in [
                    "/api/",
                    "/api/missing",
                    "/api/missing/nested",
                    "/%61pi/missing",
                    "/apps",
                    "/apps/default/missing",
                    "/assets",
                    "/assets/missing.js",
                    "/assets/missing",
                    "/assets%2fmissing",
                    "/missing.js",
                    "/missing.css",
                    "/missing.json",
                    "/favicon.ico",
                    "/missing%2Ejs",
                ] {
                    let response = app
                        .clone()
                        .oneshot(web_request(method.clone(), path, accept))
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::NOT_FOUND,
                        "{method} {path} {accept:?}"
                    );
                }
            }
        }
        let response = app
            .clone()
            .oneshot(web_request(Method::POST, "/search", Some("text/html")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = app
            .oneshot(web_request(Method::GET, "/assets/existing.js", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"export {};");
    }

    #[tokio::test]
    async fn spa_fallback_keeps_root_negotiation_and_explicit_html_dotted_routes() {
        let (_home, app) = web_router();
        for accept in [
            None,
            Some("*/*"),
            Some("application/json"),
            Some("text/html;q=0, */*"),
        ] {
            let response = app
                .clone()
                .oneshot(web_request(Method::GET, "/", accept))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
            let response = app
                .clone()
                .oneshot(web_request(Method::GET, "/api", accept))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let payload: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(payload["ok"], true);
        }
        for path in ["/", "/api", "/agents/agent.v1", "/templates/template.v1"] {
            let response = app
                .clone()
                .oneshot(web_request(Method::GET, path, Some("text/html")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()[CONTENT_TYPE], "text/html");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], TEST_INDEX.as_bytes());
        }
        let response = app
            .oneshot(web_request(Method::GET, "/agents/agent.v1", Some("*/*")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    fn if_none_match_headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            IF_NONE_MATCH,
            HeaderValue::from_str(value).expect("valid header value"),
        );
        headers
    }

    #[test]
    fn cache_control_follows_hashed_asset_layout() {
        assert_eq!(
            web_asset_cache_control("assets/index-Dz3cIIu_.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(web_asset_cache_control("index.html"), "no-cache");
        assert_eq!(web_asset_cache_control("favicon.svg"), "no-cache");
    }

    #[test]
    fn asset_response_carries_content_type_and_cache_headers() {
        let bytes = b"<html></html>".to_vec();
        let response = asset_response(bytes.clone(), "index.html", &HeaderMap::new(), false);
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers[CONTENT_TYPE].to_str().unwrap(), "text/html");
        assert_eq!(headers[CACHE_CONTROL].to_str().unwrap(), "no-cache");
        assert_eq!(headers[ETAG].to_str().unwrap(), etag_for_bytes(&bytes));

        let head = asset_response(bytes, "index.html", &HeaderMap::new(), true);
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[CACHE_CONTROL].to_str().unwrap(), "no-cache");
    }

    #[test]
    fn asset_response_revalidates_with_weak_if_none_match() {
        let bytes = b"<html></html>".to_vec();
        let etag = etag_for_bytes(&bytes);
        let not_modified = asset_response(
            bytes,
            "index.html",
            &if_none_match_headers(&format!("W/{etag}")),
            false,
        );
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            not_modified.headers()[ETAG].to_str().unwrap(),
            etag.as_str()
        );
        assert_eq!(
            not_modified.headers()[CACHE_CONTROL].to_str().unwrap(),
            "no-cache"
        );
        assert!(not_modified.headers().get(CONTENT_TYPE).is_none());
    }

    #[test]
    fn immutable_assets_still_honor_conditional_requests() {
        let bytes = b"console.log(1);\n".to_vec();
        let etag = etag_for_bytes(&bytes);
        let response = asset_response(
            bytes,
            "assets/app-CjQ9ZaBq.js",
            &if_none_match_headers(&etag),
            false,
        );
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            response.headers()[CACHE_CONTROL].to_str().unwrap(),
            "public, max-age=31536000, immutable"
        );
    }
}
