use std::io::Read;
use std::path::Path as FsPath;
use std::time::SystemTime;

use super::*;
use axum::http::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_NONE_MATCH,
    IF_RANGE, LAST_MODIFIED, RANGE, X_CONTENT_TYPE_OPTIONS,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;

/// Maximum bytes to read for text file content before truncating.
const READ_LIMIT_BYTES: usize = 1024 * 1024; // 1 MB
/// Maximum bytes to sniff for content-based MIME detection.
const SNIFF_LIMIT_BYTES: usize = 8000;

#[derive(Debug, Deserialize)]
pub(crate) struct FileQueryParams {
    #[serde(default)]
    execution_root_id: Option<String>,
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    download: Option<bool>,
    #[serde(default)]
    meta: Option<bool>,
}

#[derive(Debug, Serialize)]
struct DirectoryEntry {
    name: String,
    #[serde(rename = "type")]
    entry_type: &'static str,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
}

#[derive(Debug, Serialize)]
struct DirectoryListing {
    #[serde(rename = "type")]
    entry_type: &'static str,
    path: String,
    workspace_id: String,
    entries: Vec<DirectoryEntry>,
}

#[derive(Debug, Serialize)]
struct FileMetadata {
    #[serde(rename = "type")]
    entry_type: &'static str,
    path: String,
    workspace_id: String,
    size: u64,
    mime_type: String,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_size: Option<u64>,
}

#[derive(Debug, Serialize)]
struct FileContent {
    #[serde(flatten)]
    metadata: FileMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

/// Resolve a workspace by id and determine the execution root to browse.
async fn resolve_workspace_root(
    state: &AppState,
    workspace_id: &str,
    root_id: Option<&str>,
) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    let entries = state.host.workspace_entries().map_err(error_response)?;
    let workspace = entries
        .iter()
        .find(|entry| entry.workspace_id == workspace_id)
        .ok_or_else(|| not_found(format!("workspace '{workspace_id}' not found")))?;

    // Resolve the filesystem root from server-side state, never from the
    // client-provided root_id string. The root_id is treated as an opaque
    // server-issued token.
    let root = match root_id {
        // No root_id: use the workspace anchor from the registry.
        None => workspace.workspace_anchor.clone(),
        // canonical_root:{workspace_id} also resolves to the anchor. The
        // path comes from the server-side registry, not the client string.
        Some(id) if id.starts_with("canonical_root:") => workspace.workspace_anchor.clone(),
        // For git_worktree_root and any other format, look up the root_id
        // in the execution root registry (runtime_db). The embedded path
        // in the ID is never trusted or parsed.
        Some(id) => {
            let repo = state.host.runtime_db().execution_root_entries();
            match repo.get(id).map_err(error_response)? {
                Some(entry) if entry.removed_at.is_some() => {
                    return Err((
                        StatusCode::GONE,
                        Json(json!({
                            "error": "execution root has been removed",
                            "execution_root_id": id,
                        })),
                    ));
                }
                Some(entry) => {
                    if entry.workspace_id != workspace_id {
                        return Err(forbidden(format!(
                            "execution_root_id does not belong to workspace '{workspace_id}'"
                        )));
                    }
                    entry.filesystem_path
                }
                None => {
                    return Err(not_found(format!(
                        "execution_root_id not found in registry: '{id}'"
                    )));
                }
            }
        }
    };

    if !root.exists() {
        return Err(not_found(format!(
            "workspace root does not exist on disk: {}",
            root.display()
        )));
    }

    Ok(root)
}

/// Resolve and validate a relative path within the workspace root.
fn resolve_and_validate_path(
    root: &FsPath,
    relative: &str,
) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    let candidate = root.join(relative);
    let normalized =
        crate::system::workspace::normalize_path(&candidate).map_err(error_response)?;
    let normalized_root = crate::system::workspace::normalize_path(root).map_err(error_response)?;
    if !normalized.starts_with(&normalized_root) {
        return Err(forbidden("path escapes workspace root"));
    }

    // Canonicalize to resolve symlinks, then re-check containment.
    // This prevents symlink-based escapes that pass the lexical check above
    // but resolve outside the workspace root on disk.
    if let (Ok(canonical), Ok(canonical_root)) = (
        std::fs::canonicalize(&normalized),
        std::fs::canonicalize(&normalized_root),
    ) {
        if !canonical.starts_with(&canonical_root) {
            return Err(forbidden("path escapes workspace root (symlink)"));
        }
    }

    Ok(normalized)
}

/// Infer MIME type from file extension.
fn guess_mime(path: &FsPath) -> String {
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
fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime == "application/json"
        || mime == "application/javascript"
        || mime == "application/xml"
        || mime == "application/x-yaml"
        || mime == "application/x-sh"
        || mime == "application/x-toml"
}

/// Inclusive byte range resolved from a `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    end: u64,
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

/// MIME types that execute scripts or host active content when rendered
/// inline. Served with a sandboxing CSP so direct same-origin navigation
/// cannot run workspace-controlled scripts.
fn needs_script_sandbox(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "text/html" | "application/xhtml+xml" | "image/svg+xml" | "application/xml" | "text/xml"
    ) || mime_type.ends_with("+xml")
}

/// Content-Type used when serving raw bytes directly to a browser. Non-standard
/// text subtypes used only for JSON negotiation render as plain text, and
/// text responses get an explicit UTF-8 charset.
fn served_content_type(mime_type: &str) -> String {
    match mime_type {
        "text/typescript" | "text/tsx" => "text/plain; charset=utf-8".to_string(),
        m if m.starts_with("text/") => format!("{m}; charset=utf-8"),
        m => m.to_string(),
    }
}

/// Build a `Content-Disposition` value for a download. ASCII filenames use the
/// quoted form with quoting characters replaced; other filenames use the
/// RFC 5987/8187 extended form.
fn attachment_disposition(filename: &str) -> String {
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

/// Stream a file's raw bytes with Range, ETag/Last-Modified, and safe inline
/// disposition handling. Used for binary files, explicit downloads, and
/// direct-link (non-JSON) access to text files.
#[allow(clippy::too_many_arguments)]
async fn serve_file_bytes(
    full_path: &FsPath,
    relative_path: &str,
    mime_type: &str,
    file_size: u64,
    modified: Option<SystemTime>,
    want_download: bool,
    headers: &HeaderMap,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    let etag = build_etag(relative_path, file_size, modified);
    let last_modified = modified.map(http_date);

    if let Some(if_none_match) = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        if if_none_match_matches(if_none_match, &etag) {
            let mut builder = Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(ETAG, etag.as_str())
                .header(ACCEPT_RANGES, "bytes");
            if let Some(lm) = &last_modified {
                builder = builder.header(LAST_MODIFIED, lm.as_str());
            }
            return builder
                .body(Body::empty())
                .map_err(|err| error_response(anyhow!(err)));
        }
    }

    let mut range = RangeOutcome::Full;
    if let Some(range_header) = headers.get(RANGE).and_then(|value| value.to_str().ok()) {
        let authorized = match headers.get(IF_RANGE).and_then(|value| value.to_str().ok()) {
            Some(if_range) => if_range_matches(if_range, &etag, last_modified.as_deref()),
            None => true,
        };
        if authorized {
            range = parse_range_header(range_header, file_size);
        }
    }

    let (status, start, length, content_range) = match range {
        RangeOutcome::Full => (StatusCode::OK, 0u64, file_size, None),
        RangeOutcome::Partial(byte_range) => {
            let length = byte_range.end - byte_range.start + 1;
            (
                StatusCode::PARTIAL_CONTENT,
                byte_range.start,
                length,
                Some(format!(
                    "bytes {}-{}/{}",
                    byte_range.start, byte_range.end, file_size
                )),
            )
        }
        RangeOutcome::Unsatisfiable => {
            let response = Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{file_size}"))
                .header(ACCEPT_RANGES, "bytes")
                .header(X_CONTENT_TYPE_OPTIONS, "nosniff")
                .body(Body::empty())
                .map_err(|err| error_response(anyhow!(err)))?;
            return Ok(response);
        }
    };

    let file = tokio::fs::File::open(full_path)
        .await
        .map_err(|err| error_response(anyhow!(err)))?;
    let reader = if start > 0 {
        let mut file = file;
        file.seek(SeekFrom::Start(start))
            .await
            .map_err(|err| error_response(anyhow!(err)))?;
        file.take(length)
    } else {
        file.take(length)
    };
    let stream = ReaderStream::with_capacity(reader, 64 * 1024);

    let content_type = if want_download {
        mime_type.to_string()
    } else {
        served_content_type(mime_type)
    };
    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header(CONTENT_LENGTH, length.to_string())
        .header(ACCEPT_RANGES, "bytes")
        .header(ETAG, etag.as_str())
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff");
    if let Some(lm) = &last_modified {
        builder = builder.header(LAST_MODIFIED, lm.as_str());
    }
    if let Some(content_range) = content_range {
        builder = builder.header(CONTENT_RANGE, content_range);
    }
    if want_download {
        let filename = full_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "download".to_string());
        builder = builder.header(CONTENT_DISPOSITION, attachment_disposition(&filename));
    } else if needs_script_sandbox(mime_type) {
        builder = builder.header("Content-Security-Policy", "sandbox");
    }

    builder
        .body(Body::from_stream(stream))
        .map_err(|err| error_response(anyhow!(err)))
}

/// Handler for workspace root (no sub-path).
pub(crate) async fn workspace_files_root(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Query(params): Query<FileQueryParams>,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    workspace_files_inner(state, headers, workspace_id, String::new(), params).await
}

/// Handler for a specific path within a workspace.
pub(crate) async fn workspace_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((workspace_id, path)): Path<(String, String)>,
    Query(params): Query<FileQueryParams>,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    // {*path} in axum 0.8 captures the rest of the URL including the leading '/'.
    let path = path.trim_start_matches('/').to_string();
    workspace_files_inner(state, headers, workspace_id, path, params).await
}

async fn workspace_files_inner(
    state: Arc<AppState>,
    headers: HeaderMap,
    workspace_id: String,
    path: String,
    params: FileQueryParams,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;

    let root = resolve_workspace_root(
        &state,
        &workspace_id,
        params.root.or(params.execution_root_id).as_deref(),
    )
    .await?;
    let full_path = resolve_and_validate_path(&root, &path)?;

    let relative = path.trim_start_matches('/');

    // Directory listing
    if full_path.is_dir() {
        let mut entries = Vec::new();
        let reader = match std::fs::read_dir(&full_path) {
            Ok(r) => r,
            Err(err) => {
                return Err(error_response(anyhow!(err)));
            }
        };
        for entry in reader {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    return Err(error_response(anyhow!(err)));
                }
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let (entry_type, size) = if file_type.is_dir() {
                ("directory", 0u64)
            } else if file_type.is_symlink() {
                ("symlink", entry.metadata().map(|m| m.len()).unwrap_or(0))
            } else {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                ("file", size)
            };
            let mime_type = if file_type.is_file() {
                Some(guess_mime(&entry.path()))
            } else {
                None
            };
            entries.push(DirectoryEntry {
                name,
                entry_type,
                size,
                mime_type,
            });
        }
        entries.sort_by(|a, b| match (a.entry_type, b.entry_type) {
            ("directory", "directory") => a.name.cmp(&b.name),
            ("directory", _) => std::cmp::Ordering::Less,
            (_, "directory") => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });
        let listing = DirectoryListing {
            entry_type: "directory",
            path: relative.to_string(),
            workspace_id,
            entries,
        };
        return Ok(Json(json!(listing)).into_response());
    }

    // File path that doesn't exist
    if !full_path.exists() && !full_path.is_symlink() {
        return Err(not_found(format!("file not found: {relative}")));
    }

    // File metadata
    let metadata = tokio::fs::metadata(&full_path)
        .await
        .map_err(|err| error_response(anyhow!(err)))?;
    let file_size = metadata.len();
    let mime_type = guess_mime(&full_path);
    let want_meta = params.meta.unwrap_or(false);
    let want_download = params.download.unwrap_or(false);

    // Metadata-only response
    if want_meta {
        let meta = FileMetadata {
            entry_type: "file",
            path: relative.to_string(),
            workspace_id,
            size: file_size,
            mime_type: mime_type.clone(),
            truncated: false,
            total_size: None,
        };
        return Ok(Json(json!(meta)).into_response());
    }

    let accept_json = headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/json"))
        .unwrap_or(false);

    // Raw byte serving: binary files, explicit downloads, and direct-link
    // access to text files (no JSON negotiation). Streams from disk with
    // Range and conditional-request support instead of buffering the file.
    if !is_text_mime(&mime_type) || want_download || !accept_json {
        let modified = metadata.modified().ok();
        return serve_file_bytes(
            &full_path,
            relative,
            &mime_type,
            file_size,
            modified,
            want_download,
            &headers,
        )
        .await;
    }

    // Text preview via JSON negotiation: read with truncation
    let bytes = tokio::fs::read(&full_path)
        .await
        .map_err(|err| error_response(anyhow!(err)))?;
    let total_size = bytes.len();
    let truncated = total_size > READ_LIMIT_BYTES;
    let read_bytes = if truncated {
        &bytes[..READ_LIMIT_BYTES]
    } else {
        &bytes
    };
    // Find the largest valid UTF-8 boundary at or before READ_LIMIT_BYTES
    // to avoid splitting multi-byte characters.
    let content = if truncated {
        // Find the largest valid UTF-8 boundary at or before READ_LIMIT_BYTES
        // to avoid splitting multi-byte characters. A valid boundary is at
        // a byte that is not a UTF-8 continuation byte (0x80–0xBF).
        let mut end = READ_LIMIT_BYTES;
        while end > 0 {
            let prev = read_bytes[end - 1];
            // Continuation bytes are 0x80..=0xBF; backing up past one means
            // we're inside a multi-byte sequence.
            if !(0x80..=0xBF).contains(&prev) {
                break;
            }
            end -= 1;
        }
        String::from_utf8_lossy(&read_bytes[..end]).to_string()
    } else {
        String::from_utf8_lossy(read_bytes).to_string()
    };

    // Reaching here implies accept_json: non-JSON text access is routed to
    // serve_file_bytes above.
    let file_content = FileContent {
        metadata: FileMetadata {
            entry_type: "file",
            path: relative.to_string(),
            workspace_id,
            size: content.len() as u64,
            mime_type: mime_type.clone(),
            truncated,
            total_size: if truncated {
                Some(total_size as u64)
            } else {
                None
            },
        },
        content: Some(content),
    };
    Ok(Json(json!(file_content)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

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
