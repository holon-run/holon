//! File transport shared by Local Apps and workspace downloads.
//! Callers retain responsibility for authorization, paths, limits and policy.

use axum::{
    body::Body,
    http::{header::*, HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
};
use std::io::{self, Read};
use std::path::Path;
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
                            target: "holon::http::file_response",
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
                        tracing::warn!(target: "holon::http::file_response", %error, "file response read failed");
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

pub struct FileResponseOptions {
    pub content_type: String,
    pub disposition: Option<String>,
    pub csp: Option<HeaderValue>,
    pub cache_control: Option<HeaderValue>,
}

/// Metadata belongs to the supplied handle, not a second pathname lookup.
/// The body owns the permit until EOF, error or disconnect; bodyless replies
/// release it immediately. HEAD deliberately ignores Range (GET-only).
#[allow(clippy::too_many_arguments)]
pub async fn serve_file(
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

const SNIFF_LIMIT_BYTES: usize = 8000;

/// Infer MIME type from file extension.
pub fn guess_mime(path: &Path) -> String {
    // Override extensions that mime_guess maps to non-text types
    // (e.g., .ts -> video/mp2t, which prevents inline text rendering).
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if let Some(mime) = custom_mime_for_ext(&ext.to_lowercase()) {
            return mime.to_string();
        }
    }
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    if mime != "application/octet-stream" {
        return mime;
    }
    // Extension-based detection failed; sniff content.
    match std::fs::File::open(path) {
        Ok(mut file) => {
            let mut buf = vec![0u8; SNIFF_LIMIT_BYTES];
            match file.read(&mut buf) {
                Ok(0) => "text/plain".to_string(),
                Ok(n) => {
                    if sniff_is_text(&buf[..n]) {
                        "text/plain".to_string()
                    } else {
                        mime
                    }
                }
                Err(_) => mime,
            }
        }
        Err(_) => mime,
    }
}

/// Heuristic content sniff: returns `true` if the bytes look like text.
///
/// Uses the same approach as git/file(1): a NUL byte or a high proportion
/// of non-printable control characters (excluding \t \n \r) indicates binary.
fn sniff_is_text(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if data.contains(&0x00) {
        return false;
    }
    // Invalid UTF-8 → binary (catches most non-text formats).
    if std::str::from_utf8(data).is_err() {
        return false;
    }
    let non_printable = data
        .iter()
        .filter(|&&b| (b < 0x20 || b == 0x7f) && b != 0x09 && b != 0x0a && b != 0x0d)
        .count();
    (non_printable as f64 / data.len() as f64) < 0.30
}

/// Override MIME types for file extensions that `mime_guess` maps to
/// non-text types. The notable case is `.ts`, which the IANA registry
/// maps to `video/mp2t` (MPEG-2 Transport Stream) rather than TypeScript.
/// Without this override, the server streams raw bytes for `.ts` files,
/// causing the web GUI's JSON content negotiation to fail.
fn custom_mime_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "ts" => Some("text/typescript"),
        "tsx" => Some("text/tsx"),
        _ => None,
    }
}

/// Determine whether a MIME type represents a text file suitable for inline reading.
pub fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime == "application/json"
        || mime == "application/javascript"
        || mime == "application/xml"
        || mime == "application/x-yaml"
        || mime == "application/x-sh"
        || mime == "application/x-toml"
}

/// MIME types that execute scripts or host active content when rendered
/// inline. Served with a sandboxing CSP so direct same-origin navigation
/// cannot run workspace-controlled scripts.
pub fn needs_script_sandbox(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "text/html" | "application/xhtml+xml" | "image/svg+xml" | "application/xml" | "text/xml"
    ) || mime_type.ends_with("+xml")
}

/// Content-Type used when serving raw bytes directly to a browser. Non-standard
/// text subtypes used only for JSON negotiation render as plain text, and
/// text responses get an explicit UTF-8 charset.
pub fn served_content_type(mime_type: &str) -> String {
    match mime_type {
        "text/typescript" | "text/tsx" => "text/plain; charset=utf-8".to_string(),
        m if m.starts_with("text/") => format!("{m}; charset=utf-8"),
        m => m.to_string(),
    }
}

/// Build a `Content-Disposition` value for a download. ASCII filenames use the
/// quoted form with quoting characters replaced; other filenames use the
/// RFC 5987/8187 extended form.
pub fn attachment_disposition(filename: &str) -> String {
    if filename.is_ascii() {
        let safe: String = filename
            .chars()
            .map(|c| match c {
                '"' | '\\' | '\r' | '\n' => '_',
                other => other,
            })
            .collect();
        format!("attachment; filename=\"{safe}\"")
    } else {
        let encoded: String = filename
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#'
                            | b'$'
                            | b'&'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
                {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        format!("attachment; filename*=UTF-8''{encoded}")
    }
}

/// MIME describes a file, not whether it may be published. Unknown files are
/// opaque downloads; nothing on this route executes server-side code.
pub fn app_asset_content_type(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    match extension.to_ascii_lowercase().as_str() {
        "html" | "htm" => Some("text/html; charset=utf-8"),
        "js" | "mjs" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "json" | "map" => Some("application/json; charset=utf-8"),
        "txt" => Some("text/plain; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "ico" => Some("image/x-icon"),
        "woff" => Some("font/woff"),
        "woff2" => Some("font/woff2"),
        "ttf" => Some("font/ttf"),
        "otf" => Some("font/otf"),
        "wasm" => Some("application/wasm"),
        "data" => Some("application/octet-stream"),
        _ => None,
    }
    .map(str::to_string)
    .unwrap_or_else(|| {
        mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string()
    })
}

#[cfg(test)]
mod tests;
