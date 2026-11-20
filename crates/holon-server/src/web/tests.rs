use super::*;
use axum::http::{header::IF_NONE_MATCH, HeaderValue};

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

fn generated_artifact(relative_path: &str) -> Option<Vec<u8>> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match std::fs::read(repository.join(relative_path)) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("could not read {relative_path}: {error}"),
    }
}

#[tokio::test]
async fn generated_web_and_app_sdk_bundles_use_the_repository_build_paths() {
    let response = web_asset_response(None, "index.html", &HeaderMap::new(), false).await;
    match generated_artifact("web-gui/app/dist/index.html") {
        Some(expected) => {
            let response = response.expect("generated Web index must be served");
            assert_eq!(response.headers()[CONTENT_TYPE], "text/html");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(bytes, expected);
        }
        None => assert!(response.is_none()),
    }

    let response = app_sdk_response("test-version");
    match generated_artifact("packages/app-sdk/dist/holon.js") {
        Some(expected) => {
            let response = response.expect("generated App SDK must be served");
            assert_eq!(
                response.headers()["x-holon-app-sdk-version"],
                "test-version"
            );
            assert_eq!(
                response.headers()[CONTENT_TYPE],
                "application/javascript; charset=utf-8"
            );
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(bytes, expected);
        }
        None => assert!(response.is_none()),
    }
}
