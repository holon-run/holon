use std::time::Duration;

use axum::{
    http::{
        header::{CACHE_CONTROL, ETAG, IF_NONE_MATCH},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::{IntoResponse, Response as AxumResponse},
};

const HTTP_SLOW_RESPONSE_WARN_AFTER: Duration = Duration::from_secs(2);
const HTTP_LARGE_RESPONSE_WARN_BYTES: usize = 128 * 1024;

pub(crate) fn is_slow_or_large_response(elapsed: Duration, bytes: Option<usize>) -> bool {
    elapsed >= HTTP_SLOW_RESPONSE_WARN_AFTER
        || bytes.is_some_and(|size| size >= HTTP_LARGE_RESPONSE_WARN_BYTES)
}

pub fn warn_if_slow_json_response(route: &'static str, build_elapsed: Duration, bytes: usize) {
    if is_slow_or_large_response(build_elapsed, Some(bytes)) {
        tracing::warn!(
            target: "holon::http",
            route,
            handler_build_ms = build_elapsed.as_millis() as u64,
            response_bytes = bytes,
            "large or slow HTTP JSON payload built"
        );
    }
}

pub async fn prevent_agent_read_caching(mut response: AxumResponse) -> AxumResponse {
    // URLs outlive agent incarnations; even a tombstone must not be cached.
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub async fn add_retry_after_to_service_unavailable(mut response: AxumResponse) -> AxumResponse {
    if response.status() == StatusCode::SERVICE_UNAVAILABLE
        && !response
            .headers()
            .contains_key(axum::http::header::RETRY_AFTER)
    {
        response.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_static("1"),
        );
    }
    response
}

/// Strong ETag derived from the serialized response body. Content-addressed
/// so any change to the payload (epoch, seq, rendering) yields a new tag.
pub fn etag_for_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut etag = String::with_capacity(26);
    etag.push('"');
    for byte in digest.iter().take(12) {
        etag.push_str(&format!("{byte:02x}"));
    }
    etag.push('"');
    etag
}

/// RFC 9110 If-None-Match evaluation: satisfied when any listed entity tag
/// weakly matches the current one, or when the client sent `*`. If-None-Match
/// always uses weak comparison, so `W/"x"` matches `"x"`.
pub fn if_none_match_satisfied(headers: &HeaderMap, etag: &str) -> bool {
    let Some(raw) = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    fn opaque_tag(tag: &str) -> &str {
        tag.strip_prefix("W/").unwrap_or(tag)
    }
    let current = opaque_tag(etag);
    raw.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || opaque_tag(candidate) == current
    })
}

pub fn not_modified_response(etag: String) -> AxumResponse {
    (StatusCode::NOT_MODIFIED, [(ETAG, etag.as_str())]).into_response()
}

#[cfg(test)]
mod tests;
