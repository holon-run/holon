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
        if !request_path.is_empty() {
            if let Some(response) = web_asset_response(&state, request_path, head_only).await {
                return response;
            }
        }
        if accepts_html(&headers) {
            if let Some(response) = web_asset_response(&state, "index.html", head_only).await {
                return response;
            }
        }
    }
    not_found("Not Found").into_response()
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
    head_only: bool,
) -> Option<AxumResponse> {
    let path = normalize_web_asset_path(request_path)?;
    let bytes = if let Some(web_dist) = &state.web_dist {
        tokio::fs::read(web_dist.join(&path)).await.ok()?
    } else {
        EmbeddedWebAssets::get(&path)?.data.into_owned()
    };
    let content_type = mime_guess::from_path(&path).first_or_octet_stream();
    let body = if head_only {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type.as_ref())
        .body(body)
        .ok()
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
