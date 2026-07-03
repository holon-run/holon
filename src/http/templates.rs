use super::*;
use std::path::Path as FsPath;

const USER_GLOBAL_LIBRARY_LABEL: &str = "user_global";

/// `GET /templates/catalog`
///
/// List all globally visible templates (builtin + user global library).
/// Agent-scoped templates are excluded from this global catalog endpoint.
pub async fn templates_catalog(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;

    let config = state.host.config();
    let user_home = crate::agent_template::user_home_dir().ok();
    // Use a non-existent agent_home path so the global catalog shows only
    // builtin + user_global entries, parallel to /skills/catalog.
    let catalog = crate::agent_template::discover_agent_templates_catalog(
        user_home.as_deref(),
        FsPath::new("/nonexistent-agent-home"),
    );
    let remote_sources = crate::agent_template::effective_agent_template_remote_sources(
        &config.stored_config.agent_templates,
    );
    let remote = crate::agent_template::load_remote_template_catalog_snapshot(
        state.host.runtime_db(),
        &remote_sources,
    )
    .map_err(error_response)?;
    let mut catalog = catalog;
    catalog.extend(remote.catalog);
    Ok(Json(json!({
        "ok": true,
        "catalog": catalog,
        "sources": remote.sources,
        "diagnostics": remote.diagnostics,
    })))
}

/// `POST /templates/remote-sources/sync`
///
/// Queue a daemon job that synchronizes configured remote template sources.
pub async fn sync_template_remote_sources(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::SyncTemplateRemoteSourcesRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    super::jobs::create_template_remote_source_sync_job(state, request).await
}

/// `GET /templates/catalog/{catalog_id}`
///
/// Return template detail with full AGENTS.md content, manifest summary,
/// and skill dependencies.
pub async fn template_detail(
    Path(catalog_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;

    let user_home = crate::agent_template::user_home_dir().ok();
    let catalog = crate::agent_template::discover_agent_templates_catalog(
        user_home.as_deref(),
        FsPath::new("/nonexistent-agent-home"),
    );
    let config = state.host.config();
    let remote_sources = crate::agent_template::effective_agent_template_remote_sources(
        &config.stored_config.agent_templates,
    );
    let remote = crate::agent_template::load_remote_template_catalog_snapshot(
        state.host.runtime_db(),
        &remote_sources,
    )
    .map_err(error_response)?;
    let mut catalog = catalog;
    catalog.extend(remote.catalog);
    let Some(entry) = catalog
        .iter()
        .find(|entry| entry.catalog_id == catalog_id || entry.template == catalog_id)
    else {
        return Err(not_found(format!("template {catalog_id} not found")));
    };

    let detail = if entry.source == crate::types::AgentTemplateSourceKind::Remote {
        crate::agent_template::resolve_remote_agent_template_detail(entry)
            .await
            .map_err(error_response)?
    } else {
        let Some(detail) = crate::agent_template::resolve_agent_template_detail(entry) else {
            return Err(not_found(format!(
                "template {catalog_id} detail could not be resolved"
            )));
        };
        detail
    };

    Ok(Json(json!({
        "ok": true,
        "detail": detail,
    })))
}

/// `POST /templates/catalog/check`
///
/// Validate a local template directory without applying it.
pub async fn check_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::CheckTemplateRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;

    // Handle github_url validation (URL format check only, no download).
    if let Some(url) = request.github_url.as_deref().filter(|s| !s.is_empty()) {
        let url_owned = url.to_string();
        let result = tokio::task::spawn_blocking(move || {
            let canonical = crate::agent_template::validate_github_template_url(&url_owned)?;
            Ok::<_, anyhow::Error>(TemplateCheckResult {
                valid: true,
                errors: vec![],
                warnings: vec![format!("URL validated as: {canonical}")],
                manifest: None,
            })
        })
        .await
        .map_err(|err| error_response(anyhow!("check worker failed: {err}")))?
        .map_err(error_response)?;

        return Ok(Json(json!({
            "ok": true,
            "valid": result.valid,
            "errors": result.errors,
            "warnings": result.warnings,
            "manifest": result.manifest,
        })));
    }

    let Some(path_str) = request.path.as_deref().filter(|s| !s.is_empty()) else {
        return Err(error_response(anyhow!(
            "either path or github_url is required"
        )));
    };

    let template_dir = std::path::PathBuf::from(path_str);
    let result = tokio::task::spawn_blocking(move || {
        let canonical = std::fs::canonicalize(&template_dir)
            .map_err(|e| anyhow!("failed to resolve path: {e}"))?;
        if !canonical.is_dir() {
            return Err(anyhow!("path is not a directory: {}", canonical.display()));
        }
        check_template_dir(&canonical)
    })
    .await
    .map_err(|err| error_response(anyhow!("check worker failed: {err}")))?
    .map_err(error_response)?;

    Ok(Json(json!({
        "ok": true,
        "valid": result.valid,
        "errors": result.errors,
        "warnings": result.warnings,
        "manifest": result.manifest,
    })))
}

/// `POST /control/templates/install`
///
/// Install a template package from a GitHub tree URL into the user global
/// library.
pub async fn install_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::InstallTemplateRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;

    let github_url = request.github_url.trim().to_string();
    if github_url.is_empty() {
        return Err(error_response(anyhow!("github_url must not be empty")));
    }

    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let url = github_url.clone();
    let template_id = tokio::task::spawn_blocking(move || {
        crate::agent_template::install_template_from_github(&user_home, &url)
    })
    .await
    .map_err(|err| error_response(anyhow!("install worker failed: {err}")))?
    .map_err(template_install_error_response)?;

    Ok(Json(json!({
        "ok": true,
        "template_id": template_id,
        "library": USER_GLOBAL_LIBRARY_LABEL,
    })))
}

/// `POST /control/templates/remove`
///
/// Remove a template from the user global library.
pub async fn remove_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::RemoveTemplateRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;

    let template_id = request.template_id.trim().to_string();
    if template_id.is_empty() {
        return Err(error_response(anyhow!("template_id must not be empty")));
    }

    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let id = template_id.clone();
    tokio::task::spawn_blocking(move || {
        crate::agent_template::remove_user_template(&user_home, &id)
    })
    .await
    .map_err(|err| error_response(anyhow!("remove worker failed: {err}")))?
    .map_err(error_response)?;

    Ok(Json(json!({
        "ok": true,
        "template_id": template_id,
        "library": USER_GLOBAL_LIBRARY_LABEL,
    })))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct TemplateCheckResult {
    valid: bool,
    errors: Vec<String>,
    warnings: Vec<String>,
    manifest: Option<Value>,
}

fn check_template_dir(template_dir: &FsPath) -> anyhow::Result<TemplateCheckResult> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Check for AGENTS.md.
    let agents_md_path = template_dir.join("AGENTS.md");
    if !agents_md_path.exists() {
        errors.push("AGENTS.md not found".to_string());
    } else {
        match std::fs::read_to_string(&agents_md_path) {
            Ok(content) if content.trim().is_empty() => {
                warnings.push("AGENTS.md is empty".to_string());
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("failed to read AGENTS.md: {e}")),
        }
    }

    // Check for template.toml.
    let manifest = crate::agent_template::parse_template_manifest_for_api(template_dir);
    match &manifest {
        Some(m) => {
            if m.get("schema")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .is_empty()
            {
                warnings.push("template.toml schema field is empty".to_string());
            }
        }
        None => {
            warnings.push("template.toml not found or invalid".to_string());
        }
    }

    let valid = errors.is_empty();
    Ok(TemplateCheckResult {
        valid,
        errors,
        warnings,
        manifest,
    })
}

fn template_install_error_response(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "ok": false,
            "error": error.to_string(),
            "code": "template_install_failed",
        })),
    )
}
