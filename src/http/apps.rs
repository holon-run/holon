//! Local App Engine hosting: discovery, manifest validation, and static assets.
//!
//! Apps live at `<agent_home>/apps/<app-id>/`, where `<agent_home>` is
//! `<data_dir>/agents/<agent-id>`. Routes are logical only: clients send an
//! `agent_id` and `app_id`, never a physical path. Every filesystem lookup is
//! resolved from server-side agent state and re-validated after canonicalize,
//! so traversal and symlink escapes stay inside the owning agent's `apps/`
//! directory.

use std::path::{Path as FsPath, PathBuf};

use axum::http::header::{CONTENT_SECURITY_POLICY, X_CONTENT_TYPE_OPTIONS};
use tokio::fs;

use super::*;

const MANIFEST_FILE: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ASSET_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SEGMENT_LEN: usize = 128;

/// Baseline response policy for hosted apps. Apps run their own scripts and may
/// talk to the same-origin SDK endpoints, but may not embed the host page or
/// pull remote resources. Stronger isolation (separate origin, sandboxed
/// iframe, host-credential isolation) is intentionally out of scope for the
/// first Local App Engine slice.
const APP_CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'self'; form-action 'self'";

/// Router for the top-level `/apps` surface.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/{agent_id}", get(list_apps))
        .route("/{agent_id}/{app_id}", get(serve_app_entry))
        .route("/{agent_id}/{app_id}/", get(serve_app_entry))
        .route("/{agent_id}/{app_id}/{*asset_path}", get(serve_app_asset))
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    id: String,
    name: String,
    version: String,
    entry: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone)]
struct AppManifest {
    id: String,
    name: String,
    version: String,
    entry: String,
    description: Option<String>,
}

/// Discover the valid apps owned by an agent.
///
/// A valid agent with a missing or empty `apps/` directory yields an empty
/// list rather than an error, and GET never creates directories. Entries whose
/// id fails segment validation, whose directory escapes `apps/` after
/// canonicalize, or whose manifest is missing/invalid are skipped so
/// discovery only surfaces usable apps.
async fn list_apps(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    validate_segment("agent_id", &agent_id)?;

    let agent_home = state.host.agent_data_dir(&agent_id);
    let apps_dir = agent_home.join("apps");
    let mut apps = Vec::new();
    let apps_root = match canonical_apps_root(&agent_home, &apps_dir).await? {
        Some(root) => root,
        None => return Ok(Json(json!({ "agent_id": agent_id, "apps": apps }))),
    };
    let mut entries = match fs::read_dir(&apps_root).await {
        Ok(entries) => entries,
        Err(_) => return Ok(Json(json!({ "agent_id": agent_id, "apps": apps }))),
    };

    while let Some(entry) = entries.next_entry().await.map_err(internal)? {
        let file_type = match entry.file_type().await {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        let app_id = match entry.file_name().into_string() {
            Ok(app_id) => app_id,
            Err(_) => continue,
        };
        if validate_segment("app_id", &app_id).is_err() {
            continue;
        }
        let app_root = match canonical_app_root(&agent_home, &apps_dir, &app_id).await {
            Ok(root) => root,
            Err(_) => continue,
        };
        let manifest = match read_manifest(&app_root, &app_id).await {
            Ok(manifest) => manifest,
            Err(_) => continue,
        };
        apps.push(json!({
            "id": manifest.id,
            "name": manifest.name,
            "version": manifest.version,
            "entry": manifest.entry,
            "description": manifest.description,
            "url": format!("/apps/{agent_id}/{}/", manifest.id),
        }));
    }

    apps.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    Ok(Json(json!({ "agent_id": agent_id, "apps": apps })))
}

async fn serve_app_entry(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((agent_id, app_id)): Path<(String, String)>,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    let app_root = resolve_app(&state, &headers, &agent_id, &app_id).await?;
    let manifest = read_manifest(&app_root, &app_id).await?;
    serve_relative(&app_root, &manifest.entry).await
}

async fn serve_app_asset(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((agent_id, app_id, asset_path)): Path<(String, String, String)>,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    let app_root = resolve_app(&state, &headers, &agent_id, &app_id).await?;
    // Serving assets still requires a valid app manifest so discovery, entry,
    // and asset access share one definition of "this is an app".
    let _manifest = read_manifest(&app_root, &app_id).await?;
    serve_relative(&app_root, asset_path.trim_start_matches('/')).await
}

async fn resolve_app(
    state: &AppState,
    headers: &HeaderMap,
    agent_id: &str,
    app_id: &str,
) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    authorize_remote_access(headers, state).map_err(|err| auth_required(err.to_string()))?;
    validate_segment("agent_id", agent_id)?;
    validate_segment("app_id", app_id)?;
    let agent_home = state.host.agent_data_dir(agent_id);
    let apps_dir = agent_home.join("apps");
    canonical_app_root(&agent_home, &apps_dir, app_id).await
}

/// Canonicalize an app directory and prove it stays inside the agent's
/// `apps/` directory. Rejecting symlinked app directories keeps one agent from
/// serving another agent's files through a filesystem link.
async fn canonical_app_root(
    agent_home: &FsPath,
    apps_dir: &FsPath,
    app_id: &str,
) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    let apps_root = canonical_apps_root(agent_home, apps_dir)
        .await?
        .ok_or_else(|| not_found("agent apps directory not found"))?;
    let candidate = fs::canonicalize(apps_root.join(app_id))
        .await
        .map_err(|_| not_found(format!("app '{app_id}' not found")))?;
    if !candidate.starts_with(&apps_root) {
        return Err(forbidden("app directory escapes the agent apps root"));
    }
    Ok(candidate)
}

/// Resolve an agent's apps root without trusting a symlink supplied by the
/// agent. A missing root is valid for discovery, but a missing agent home is
/// not.
async fn canonical_apps_root(
    agent_home: &FsPath,
    apps_dir: &FsPath,
) -> Result<Option<PathBuf>, (StatusCode, Json<Value>)> {
    let home_metadata = fs::metadata(agent_home)
        .await
        .map_err(|_| not_found("agent not found"))?;
    if !home_metadata.is_dir() {
        return Err(not_found("agent not found"));
    }

    let apps_metadata = match fs::symlink_metadata(apps_dir).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(internal("failed to inspect agent apps directory")),
    };
    if apps_metadata.file_type().is_symlink() {
        return Err(forbidden("agent apps directory must not be a symlink"));
    }
    if !apps_metadata.is_dir() {
        return Err(forbidden("agent apps path is not a directory"));
    }

    let agent_root = fs::canonicalize(agent_home)
        .await
        .map_err(|_| not_found("agent not found"))?;
    let apps_root = fs::canonicalize(apps_dir)
        .await
        .map_err(|_| not_found("agent apps directory not found"))?;
    if !apps_root.starts_with(&agent_root) {
        return Err(forbidden("agent apps directory escapes the agent home"));
    }
    Ok(Some(apps_root))
}

async fn read_manifest(
    app_root: &FsPath,
    app_id: &str,
) -> Result<AppManifest, (StatusCode, Json<Value>)> {
    let path = app_root.join(MANIFEST_FILE);
    let metadata = fs::metadata(&path)
        .await
        .map_err(|_| unprocessable("app manifest.json is missing"))?;
    if !metadata.is_file() {
        return Err(unprocessable("app manifest.json is missing"));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(unprocessable("app manifest.json exceeds the size limit"));
    }
    let bytes = fs::read(&path).await.map_err(internal)?;
    let raw: RawManifest = serde_json::from_slice(&bytes)
        .map_err(|error| unprocessable(format!("app manifest.json is invalid: {error}")))?;

    if raw.id != app_id {
        return Err(unprocessable(
            "app manifest id must match the requested app id",
        ));
    }
    if raw.name.trim().is_empty() {
        return Err(unprocessable("app manifest name must not be empty"));
    }
    if raw.version.trim().is_empty() {
        return Err(unprocessable("app manifest version must not be empty"));
    }
    let entry = raw.entry.trim();
    let entry_path = sanitized_relative(entry)
        .map_err(|_| unprocessable("app manifest entry must be a safe relative path"))?;
    if entry_path.as_os_str().is_empty() {
        return Err(unprocessable("app manifest entry must not be empty"));
    }
    if !is_html(&entry_path) {
        return Err(unprocessable(
            "app manifest entry must reference an HTML file",
        ));
    }

    Ok(AppManifest {
        id: raw.id,
        name: raw.name.trim().to_string(),
        version: raw.version.trim().to_string(),
        entry: entry.to_string(),
        description: raw.description,
    })
}

async fn serve_relative(
    app_root: &FsPath,
    relative: &str,
) -> Result<AxumResponse, (StatusCode, Json<Value>)> {
    let relative_path = sanitized_relative(relative)?;
    if relative_path.as_os_str().is_empty() {
        return Err(not_found("app entry path is empty"));
    }

    let candidate = app_root.join(&relative_path);
    let canonical = fs::canonicalize(&candidate)
        .await
        .map_err(|_| not_found(format!("app asset '{}' not found", relative_path.display())))?;
    if !canonical.starts_with(app_root) {
        return Err(forbidden("app asset escapes the app root"));
    }
    let metadata = fs::metadata(&canonical)
        .await
        .map_err(|_| not_found("app asset not found"))?;
    if !metadata.is_file() {
        return Err(not_found("app asset is not a file"));
    }
    if metadata.len() > MAX_ASSET_BYTES {
        return Err(http_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            HttpErrorEnvelope::new("app_asset_too_large", "app asset exceeds the size limit"),
        ));
    }
    let content_type = asset_content_type(&canonical).ok_or_else(|| {
        http_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            HttpErrorEnvelope::new(
                "unsupported_app_asset_type",
                format!(
                    "app asset type is not allowed: {}",
                    canonical
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or("<none>")
                ),
            ),
        )
    })?;
    let bytes = fs::read(&canonical).await.map_err(internal)?;
    Ok(asset_response(content_type, bytes))
}

/// Validate a single logical path segment (`agent_id` or `app_id`).
fn validate_segment(label: &str, value: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if value.is_empty() || value.len() > MAX_SEGMENT_LEN {
        return Err(bad_request(format!(
            "{label} must be 1 to {MAX_SEGMENT_LEN} characters"
        )));
    }
    if value == "." || value == ".." {
        return Err(bad_request(format!(
            "{label} must not be a relative path segment"
        )));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(bad_request(format!(
            "{label} contains unsupported characters"
        )));
    }
    Ok(())
}

/// Reduce a client-supplied asset path to safe relative components.
fn sanitized_relative(raw: &str) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    let trimmed = raw.trim_start_matches('/');
    if trimmed.is_empty() {
        return Ok(PathBuf::new());
    }
    let mut safe = PathBuf::new();
    for component in FsPath::new(trimmed).components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| bad_request("app asset path must be valid UTF-8"))?;
                if part.is_empty() {
                    return Err(bad_request("app asset path contains an empty segment"));
                }
                if part.contains('\\') || part.chars().any(char::is_control) {
                    return Err(bad_request(
                        "app asset path contains unsupported characters",
                    ));
                }
                safe.push(part);
            }
            _ => return Err(forbidden("app asset path escapes the app root")),
        }
    }
    Ok(safe)
}

fn is_html(path: &FsPath) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("html" | "htm")
    )
}

/// Static asset allowlist. Executable server handlers, arbitrary methods, and
/// reverse-proxy behavior are not part of the hosting contract.
fn asset_content_type(path: &FsPath) -> Option<&'static str> {
    let extension = path.extension().and_then(|ext| ext.to_str())?;
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
        _ => None,
    }
}

fn asset_response(content_type: &'static str, bytes: Vec<u8>) -> AxumResponse {
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(APP_CSP));
    response
}

fn unprocessable(reason: impl Into<String>) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::UNPROCESSABLE_ENTITY,
        HttpErrorEnvelope::new("invalid_app_manifest", reason),
    )
}

fn internal(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    http_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        HttpErrorEnvelope::new("app_host_error", error.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_rejects_traversal_and_separators() {
        assert!(validate_segment("agent_id", "default").is_ok());
        assert!(validate_segment("app_id", "hello-world_1.0").is_ok());
        assert!(validate_segment("agent_id", "").is_err());
        assert!(validate_segment("agent_id", ".").is_err());
        assert!(validate_segment("agent_id", "..").is_err());
        assert!(validate_segment("agent_id", "a/b").is_err());
        assert!(validate_segment("agent_id", "a b").is_err());
        assert!(validate_segment("agent_id", "a\\b").is_err());
    }

    #[test]
    fn relative_path_rejects_parent_and_absolute_components() {
        assert_eq!(
            sanitized_relative("assets/app.js").unwrap(),
            PathBuf::from("assets/app.js")
        );
        assert_eq!(
            sanitized_relative("/index.html").unwrap(),
            PathBuf::from("index.html")
        );
        assert!(sanitized_relative("../secret").is_err());
        assert!(sanitized_relative("a/../../b").is_err());
        assert!(sanitized_relative("a\\..\\b").is_err());
        // Interior `.` segments are normalized away by `Path::components`.
        assert_eq!(sanitized_relative("a/./b").unwrap(), PathBuf::from("a/b"));
    }

    #[test]
    fn asset_content_type_is_allowlisted() {
        assert_eq!(
            asset_content_type(FsPath::new("index.html")),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            asset_content_type(FsPath::new("app.js")),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(asset_content_type(FsPath::new("run.sh")), None);
        assert_eq!(asset_content_type(FsPath::new("data.bin")), None);
    }
}
