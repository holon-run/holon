//! File transport shared by Local Apps and workspace downloads.
//! Callers retain responsibility for authorization, paths, limits and policy.

use axum::{
    body::Body,
    http::{header::*, HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::SystemTime;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::OwnedSemaphorePermit;
use tokio_stream::Stream;
use tokio_util::io::ReaderStream;

/// Inclusive byte range resolved from a `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    end: u64,
}

struct FileStream {
    reader: ReaderStream<tokio::io::Take<File>>,
    remaining: u64,
    permit: Option<OwnedSemaphorePermit>,
}

impl Stream for FileStream {
    type Item = io::Result<axum::body::Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.reader).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                self.remaining -= bytes.len() as u64;
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(result) => {
                // EOF/error release capacity even if a consumer retains the body.
                self.permit.take();
                match result {
                    None if self.remaining != 0 => {
                        tracing::warn!(
                            remaining = self.remaining,
                            "file response truncated during transfer"
                        );
                        self.remaining = 0;
                        Poll::Ready(Some(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "file truncated during transfer",
                        ))))
                    }
                    Some(Err(ref error)) => {
                        tracing::warn!(%error, "file response read failed");
                        Poll::Ready(result)
                    }
                    _ => Poll::Ready(result),
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Outcome of interpreting a `Range` header against a resource length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeOutcome {
    /// Serve the full representation (no usable range).
    Full,
    /// Serve the partial range with 206.
    Partial(ByteRange),
    /// Range cannot be satisfied; respond 416.
    Unsatisfiable,
}

/// Parse a single-range `bytes=` header against `len`. Malformed headers and
/// multi-range requests fall back to `Full` (the server may ignore Range).
fn parse_range_header(value: &str, len: u64) -> RangeOutcome {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeOutcome::Full;
    };
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((start_spec, end_spec)) = spec.split_once('-') else {
        return RangeOutcome::Full;
    };
    let (start, end) = if start_spec.trim().is_empty() {
        // Suffix form: last N bytes.
        let Ok(suffix) = end_spec.trim().parse::<u64>() else {
            return RangeOutcome::Full;
        };
        if suffix == 0 {
            return RangeOutcome::Unsatisfiable;
        }
        let suffix = suffix.min(len);
        (len - suffix, len.saturating_sub(1))
    } else {
        let Ok(start) = start_spec.trim().parse::<u64>() else {
            return RangeOutcome::Full;
        };
        let end = match end_spec.trim() {
            "" => len.saturating_sub(1),
            raw => {
                let Ok(end) = raw.parse::<u64>() else {
                    return RangeOutcome::Full;
                };
                end
            }
        };
        (start, end)
    };
    if len == 0 || start >= len {
        return RangeOutcome::Unsatisfiable;
    }
    if end < start {
        // Invalid byte-range-spec (RFC 9110 §14.1.2): last-byte-pos must not
        // be less than first-byte-pos. The spec is ignored so the full 200
        // body is served instead of underflowing the partial length.
        return RangeOutcome::Full;
    }
    RangeOutcome::Partial(ByteRange {
        start,
        end: end.min(len - 1),
    })
}

/// Strip whitespace and a weak validator `W/` prefix from an entity tag.
fn strip_weak_tag(value: &str) -> &str {
    value.trim().strip_prefix("W/").unwrap_or(value.trim())
}

/// Whether an `If-None-Match` header matches the current entity tag.
/// Weak comparison per RFC 9110: `W/` prefixes are ignored.
fn if_none_match_matches(if_none_match: &str, etag: &str) -> bool {
    let value = if_none_match.trim();
    if value == "*" {
        return true;
    }
    value
        .split(',')
        .any(|candidate| strip_weak_tag(candidate) == strip_weak_tag(etag))
}

/// Whether an `If-Range` header authorizes serving a range. Accepts the exact
/// entity tag or an exact Last-Modified date echo. RFC 9110 §13.1.5 requires
/// strong comparison, so client-sent weak tags never match.
fn if_range_matches(if_range: &str, etag: &str, last_modified: Option<&str>) -> bool {
    let value = if_range.trim();
    value == etag || last_modified.is_some_and(|lm| value == lm)
}

/// Build a strong entity tag from identity, size, and modification time.
/// Uses FNV-1a rather than `DefaultHasher`: the std hash algorithm is
/// unspecified and may change across Rust releases, which would silently
/// invalidate cached validators after a runtime upgrade.
fn build_etag(relative_path: &str, size: u64, modified: Option<SystemTime>) -> String {
    let mtime_nanos = modified
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in relative_path
        .as_bytes()
        .iter()
        .chain(&size.to_le_bytes())
        .chain(&mtime_nanos.to_le_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("\"{hash:x}-{mtime_nanos:x}\"")
}

/// Format a timestamp as an HTTP-date (IMF-fixdate).
fn http_date(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time)
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

pub(super) struct FileResponseOptions {
    pub content_type: String,
    pub disposition: Option<String>,
    pub csp: Option<HeaderValue>,
    pub cache_control: Option<HeaderValue>,
}

/// Metadata belongs to the supplied handle, not a second pathname lookup.
/// The body owns the permit until EOF, error or disconnect; bodyless replies
/// release it immediately. HEAD deliberately ignores Range (GET-only).
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_file(
    mut file: File,
    metadata: &std::fs::Metadata,
    identity: &str,
    headers: &HeaderMap,
    method: &Method,
    options: FileResponseOptions,
    permit: Option<OwnedSemaphorePermit>,
) -> io::Result<Response> {
    let size = metadata.len();
    let etag = build_etag(identity, size, metadata.modified().ok());
    let last_modified = metadata.modified().ok().map(http_date);
    let not_modified = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| if_none_match_matches(value, &etag));
    let range = if !not_modified && method == Method::GET {
        headers
            .get(RANGE)
            .and_then(|value| value.to_str().ok())
            .filter(|_| {
                headers
                    .get(IF_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .is_none_or(|value| if_range_matches(value, &etag, last_modified.as_deref()))
            })
            .map(|value| parse_range_header(value, size))
            .unwrap_or(RangeOutcome::Full)
    } else {
        RangeOutcome::Full
    };
    let (status, start, length, content_range) = if not_modified {
        (StatusCode::NOT_MODIFIED, 0, 0, None)
    } else {
        match range {
            RangeOutcome::Full => (StatusCode::OK, 0, size, None),
            RangeOutcome::Partial(range) => (
                StatusCode::PARTIAL_CONTENT,
                range.start,
                range.end - range.start + 1,
                Some(format!("bytes {}-{}/{size}", range.start, range.end)),
            ),
            RangeOutcome::Unsatisfiable => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                0,
                0,
                Some(format!("bytes */{size}")),
            ),
        }
    };
    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, options.content_type)
        .header(ACCEPT_RANGES, "bytes")
        .header(ETAG, etag)
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff");
    if status != StatusCode::NOT_MODIFIED {
        builder = builder.header(CONTENT_LENGTH, length.to_string());
    }
    if let Some(value) = last_modified {
        builder = builder.header(LAST_MODIFIED, value);
    }
    if let Some(value) = content_range {
        builder = builder.header(CONTENT_RANGE, value);
    }
    if let Some(value) = options.disposition {
        builder = builder.header(CONTENT_DISPOSITION, value);
    }
    if let Some(value) = options.csp {
        builder = builder.header(CONTENT_SECURITY_POLICY, value);
    }
    if let Some(value) = options.cache_control {
        builder = builder.header(CACHE_CONTROL, value);
    }
    let body = if method == Method::HEAD || length == 0 {
        Body::empty()
    } else {
        if start > 0 {
            file.seek(SeekFrom::Start(start)).await?;
        }
        let stream = ReaderStream::with_capacity(file.take(length), 64 * 1024);
        // A truncated file must abort the body rather than silently satisfy a
        // previously advertised Content-Length. take() also bounds file growth.
        Body::from_stream(FileStream {
            reader: stream,
            remaining: length,
            permit,
        })
    };
    builder.body(body).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
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
}
