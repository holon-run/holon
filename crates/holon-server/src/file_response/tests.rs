use super::*;

fn options() -> FileResponseOptions {
    FileResponseOptions {
        content_type: "application/octet-stream".into(),
        disposition: None,
        csp: None,
        cache_control: Some(HeaderValue::from_static("no-store")),
    }
}

#[tokio::test]
async fn bounded_body_releases_permits_on_eof_drop_and_read_error() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("asset");
    tokio::fs::write(&path, b"abc").await.unwrap();
    let file = File::open(&path).await.unwrap();
    let metadata = file.metadata().await.unwrap();
    // Growth after checking metadata cannot increase the sent body.
    tokio::fs::write(&path, b"abcdef").await.unwrap();
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
    let response = serve_file(
        file,
        &metadata,
        "asset",
        &HeaderMap::new(),
        &Method::GET,
        options(),
        Some(semaphore.clone().try_acquire_owned().unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(semaphore.available_permits(), 15);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"abc");
    assert_eq!(semaphore.available_permits(), 16);

    let mut bodies = Vec::new();
    for _ in 0..16 {
        let file = File::open(&path).await.unwrap();
        bodies.push(
            serve_file(
                file,
                &metadata,
                "asset",
                &HeaderMap::new(),
                &Method::GET,
                options(),
                Some(semaphore.clone().try_acquire_owned().unwrap()),
            )
            .await
            .unwrap(),
        );
    }
    assert!(semaphore.clone().try_acquire_owned().is_err());
    bodies.pop();
    assert_eq!(
        semaphore.available_permits(),
        1,
        "disconnect drops the body lease"
    );
    drop(bodies);
    assert_eq!(semaphore.available_permits(), 16);

    let file = File::open(&path).await.unwrap();
    let response = serve_file(
        file,
        &metadata,
        "asset",
        &HeaderMap::new(),
        &Method::GET,
        options(),
        Some(semaphore.clone().try_acquire_owned().unwrap()),
    )
    .await
    .unwrap();
    tokio::fs::write(&path, b"").await.unwrap();
    assert!(axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .is_err());
    assert_eq!(
        semaphore.available_permits(),
        16,
        "read error releases the lease"
    );
}

#[tokio::test]
async fn bodyless_responses_release_permits_without_reading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("asset");
    tokio::fs::write(&path, b"abc").await.unwrap();
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    for (method, range, validator, status) in [
        (Method::HEAD, "bytes=1-2", false, StatusCode::OK),
        (
            Method::GET,
            "bytes=100-",
            false,
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
        (Method::GET, "bytes=1-2", true, StatusCode::NOT_MODIFIED),
    ] {
        tokio::fs::write(&path, b"abc").await.unwrap();
        let file = File::open(&path).await.unwrap();
        let metadata = file.metadata().await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(RANGE, HeaderValue::from_static(range));
        if validator {
            headers.insert(
                IF_NONE_MATCH,
                HeaderValue::from_str(&build_etag(
                    "asset",
                    metadata.len(),
                    metadata.modified().ok(),
                ))
                .unwrap(),
            );
        }
        // A streaming body would now fail with UnexpectedEof.
        tokio::fs::write(&path, b"").await.unwrap();
        let response = serve_file(
            file,
            &metadata,
            "asset",
            &headers,
            &method,
            options(),
            Some(semaphore.clone().try_acquire_owned().unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(semaphore.available_permits(), 1);
        assert!(axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty());
    }
}
#[test]
fn parse_range_header_forms() {
    assert_eq!(
        parse_range_header("bytes=0-3", 26),
        RangeOutcome::Partial(ByteRange { start: 0, end: 3 })
    );
    assert_eq!(
        parse_range_header("bytes=20-", 26),
        RangeOutcome::Partial(ByteRange { start: 20, end: 25 })
    );
    // End beyond length is clamped.
    assert_eq!(
        parse_range_header("bytes=20-100", 26),
        RangeOutcome::Partial(ByteRange { start: 20, end: 25 })
    );
    assert_eq!(
        parse_range_header("bytes=-4", 26),
        RangeOutcome::Partial(ByteRange { start: 22, end: 25 })
    );
    // Suffix longer than the resource covers it entirely.
    assert_eq!(
        parse_range_header("bytes=0-", 26),
        RangeOutcome::Partial(ByteRange { start: 0, end: 25 })
    );
}
#[test]
fn parse_range_header_degenerate() {
    // Out of bounds and empty-suffix ranges are unsatisfiable.
    assert_eq!(
        parse_range_header("bytes=26-", 26),
        RangeOutcome::Unsatisfiable
    );
    assert_eq!(
        parse_range_header("bytes=100-200", 26),
        RangeOutcome::Unsatisfiable
    );
    assert_eq!(
        parse_range_header("bytes=-0", 26),
        RangeOutcome::Unsatisfiable
    );
    assert_eq!(
        parse_range_header("bytes=0-1", 0),
        RangeOutcome::Unsatisfiable
    );
    // Unknown units, multi-range, and malformed values fall back to full.
    assert_eq!(parse_range_header("items=0-3", 26), RangeOutcome::Full);
    assert_eq!(parse_range_header("bytes=0-1,3-4", 26), RangeOutcome::Full);
    // Reversed ranges are invalid specs, ignored per RFC 9110 §14.1.2.
    assert_eq!(parse_range_header("bytes=5-3", 26), RangeOutcome::Full);
    assert_eq!(parse_range_header("bytes=25-24", 26), RangeOutcome::Full);
    assert_eq!(parse_range_header("bytes=abc", 26), RangeOutcome::Full);
    assert_eq!(parse_range_header("bytes=", 26), RangeOutcome::Full);
}
#[test]
fn entity_tag_matchers() {
    let etag = "\"abc-123\"";
    assert!(if_none_match_matches(etag, etag));
    assert!(if_none_match_matches(&format!("W/{etag}"), etag));
    assert!(if_none_match_matches("\"other\", W/\"abc-123\"", etag));
    assert!(if_none_match_matches("*", etag));
    assert!(!if_none_match_matches("\"stale\"", etag));

    assert!(if_range_matches(etag, etag, None));
    // If-Range uses strong comparison: weak tags never match.
    assert!(!if_range_matches("W/\"abc-123\"", etag, None));
    assert!(!if_range_matches(&format!("W/{etag}"), etag, None));
    assert!(if_range_matches(
        "Sun, 06 Nov 1994 08:49:37 GMT",
        etag,
        Some("Sun, 06 Nov 1994 08:49:37 GMT")
    ));
    assert!(!if_range_matches("\"stale\"", etag, None));
    assert!(!if_range_matches(
        "Sun, 06 Nov 1994 08:49:37 GMT",
        etag,
        Some("Mon, 07 Nov 1994 08:49:37 GMT")
    ));
}
#[test]
fn build_etag_is_deterministic() {
    let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    let etag = build_etag("src/lib.rs", 1024, Some(modified));
    assert_eq!(etag, build_etag("src/lib.rs", 1024, Some(modified)));
    assert!(etag.starts_with('"') && etag.ends_with('"'));
    assert_ne!(etag, build_etag("src/other.rs", 1024, Some(modified)));
    assert_ne!(etag, build_etag("src/lib.rs", 2048, Some(modified)));
    assert_ne!(etag, build_etag("src/lib.rs", 1024, None));
}

#[test]
fn sniff_detects_text() {
    assert!(sniff_is_text(b""));
    assert!(sniff_is_text(b"Hello, world!\n"));
    assert!(sniff_is_text(b"all: build\n\tcargo build\n"));
    assert!(sniff_is_text(
        "UTF-8: \u{4e2d}\u{6587}\u{6d4b}\u{8bd5}\n".as_bytes()
    ));
    // Tabs, newlines, carriage returns are fine.
    assert!(sniff_is_text(b"col1\tcol2\r\nval1\tval2\r\n"));
}

#[test]
fn sniff_detects_binary() {
    // NUL byte → binary.
    assert!(!sniff_is_text(&[0x00, 0x01, 0x02, 0x03]));
    // PNG header.
    assert!(!sniff_is_text(&[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a
    ]));
    // ELF header.
    assert!(!sniff_is_text(&[
        0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01, 0x00
    ]));
}

#[test]
fn sniff_threshold_boundary() {
    // 3 control bytes out of 13 total ≈ 23% → still text (< 30%).
    let mut data = b"normal text\n".to_vec();
    data.extend_from_slice(&[0x01, 0x02, 0x03]);
    assert!(sniff_is_text(&data));
    // 5 control bytes out of 13 ≈ 38% → binary (>= 30%).
    let mut data = b"normal tex\n".to_vec();
    data.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05]);
    assert!(!sniff_is_text(&data));
}

#[test]
fn attachment_disposition_forms() {
    assert_eq!(
        attachment_disposition("notes.txt"),
        "attachment; filename=\"notes.txt\""
    );
    // Quote characters are replaced, never emitted raw.
    assert_eq!(
        attachment_disposition("bad\"name.txt"),
        "attachment; filename=\"bad_name.txt\""
    );
    // Non-ASCII names use the extended form.
    assert_eq!(
        attachment_disposition("笔记.txt"),
        "attachment; filename*=UTF-8''%E7%AC%94%E8%AE%B0.txt"
    );
}

#[test]
fn asset_content_type_describes_known_and_unknown_files() {
    assert_eq!(
        app_asset_content_type(Path::new("index.html")),
        "text/html; charset=utf-8"
    );
    assert_eq!(
        app_asset_content_type(Path::new("app.js")),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        app_asset_content_type(Path::new("game.wasm")),
        "application/wasm"
    );
    assert_eq!(
        app_asset_content_type(Path::new("data.bin")),
        "application/octet-stream"
    );
    assert_eq!(
        app_asset_content_type(Path::new("unknown-extension")),
        "application/octet-stream"
    );
}
