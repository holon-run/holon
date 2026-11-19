use std::path::{Component, PathBuf};

use axum::{
    body::Body,
    http::{
        header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, ETAG},
        HeaderMap, Response, StatusCode,
    },
    response::{IntoResponse, Response as AxumResponse},
};
use percent_encoding::percent_decode_str;

use crate::response::{etag_for_bytes, if_none_match_satisfied};

pub fn login_page_response(head_only: bool) -> AxumResponse {
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

pub fn accepts_html(headers: &HeaderMap) -> bool {
    html_acceptance(headers).is_some_and(|(specificity, quality)| specificity == 2 && quality > 0.0)
}

pub fn accepts_spa_html(request_path: &str, headers: &HeaderMap) -> bool {
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
pub fn asset_response(
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

pub fn normalize_web_asset_path(request_path: &str) -> Option<String> {
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
mod tests;
