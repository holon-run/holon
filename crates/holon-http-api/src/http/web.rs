use super::*;
use holon_server::web::{accepts_spa_html, login_page_response, normalize_web_asset_path};

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
            if state.host.config().auth.mode == crate::authentication::AuthenticationMode::Oidc {
                match super::authenticate_session(&headers, &state) {
                    Ok(_) => {}
                    Err(AuthenticationError::Rejected(_)) => {
                        return (
                            StatusCode::FOUND,
                            [(LOCATION, HeaderValue::from_static("/login"))],
                        )
                            .into_response();
                    }
                    Err(error @ AuthenticationError::Infrastructure(_)) => {
                        return authentication_error_response(error).into_response();
                    }
                }
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

pub(crate) async fn web_asset_response(
    state: &AppState,
    request_path: &str,
    headers: &HeaderMap,
    head_only: bool,
) -> Option<AxumResponse> {
    holon_server::web::web_asset_response(
        state.web_dist.as_deref().map(PathBuf::as_path),
        request_path,
        headers,
        head_only,
    )
    .await
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
}
