use std::path::Path as FsPath;

use super::*;

/// Maximum bytes to read for text file content before truncating.
const READ_LIMIT_BYTES: usize = 1024 * 1024; // 1 MB

#[derive(Debug, Deserialize)]
pub(crate) struct FileQueryParams {
    #[serde(default)]
    execution_root_id: Option<String>,
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
fn resolve_workspace_root(
    state: &AppState,
    workspace_id: &str,
    execution_root_id: Option<&str>,
) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    let entries = state.host.workspace_entries().map_err(error_response)?;
    let workspace = entries
        .iter()
        .find(|entry| entry.workspace_id == workspace_id)
        .ok_or_else(|| not_found(format!("workspace '{workspace_id}' not found")))?;

    // When execution_root_id is specified, reject it — we cannot resolve the
    // filesystem path from occupancy records alone (they carry only the opaque
    // id, not the path). Isolated-root browsing requires wiring up path
    // resolution from agent-scoped ActiveWorkspaceEntry data, which is future work.
    let root = if let Some(root_id) = execution_root_id {
        return Err(bad_request(format!(
            "execution_root_id resolution is not yet supported; cannot browse isolated root '{root_id}'"
        )));
    } else {
        workspace.workspace_anchor.clone()
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
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
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

    let root = resolve_workspace_root(&state, &workspace_id, params.execution_root_id.as_deref())?;
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

    // Binary/image files: stream raw bytes
    if !is_text_mime(&mime_type) || want_download {
        let bytes = tokio::fs::read(&full_path)
            .await
            .map_err(|err| error_response(anyhow!(err)))?;
        let content_type = HeaderValue::from_str(&mime_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
        let mut response = Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, content_type);
        if want_download {
            let filename = full_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "download".to_string());
            response = response.header(
                "Content-Disposition",
                HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                    .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
            );
        }
        return Ok(response
            .body(Body::from(bytes))
            .map_err(|err| error_response(anyhow!(err)))?);
    }

    // Text file: read with truncation
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

    if accept_json {
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
    } else {
        let mut response = Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, mime_type.as_str());
        if truncated {
            response = response.header("X-Content-Truncated", "true");
        }
        Ok(response
            .body(Body::from(content))
            .map_err(|err| error_response(anyhow!(err)))?)
    }
}
