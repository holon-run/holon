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
        if accepts_html(&headers) {
            if state.host.config().auth.mode == crate::authentication::AuthenticationMode::Oidc
                && super::authenticate_session(&headers, &state).is_err()
            {
                return (
                    StatusCode::FOUND,
                    [(LOCATION, HeaderValue::from_static("/login"))],
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
    headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| {
            accept
                .split(',')
                .any(|part| part.trim_start().starts_with("text/html"))
        })
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
