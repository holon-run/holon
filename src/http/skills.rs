use super::*;
use axum::extract::Query;

pub async fn list_skills(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let identity = runtime
        .agent_identity_view()
        .await
        .map_err(error_response)?;
    let skills = runtime
        .skills_runtime_view(&identity)
        .await
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skills": skills.discoverable_skills,
    })))
}

pub async fn install_skill(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::InstallSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let skill_name =
        crate::skills::install_skill_with_user_home(&agent_home, Some(&user_home), &request.kind)
            .map_err(skill_install_error_response)?;
    runtime
        .append_audit_event(
            "skill_installed",
            json!({
                "target_agent_id": agent_id,
                "skill_name": skill_name,
                "kind": request.kind,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skill_name": skill_name,
        "compat": "install",
    })))
}

pub async fn add_skill_to_catalog(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::AddSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let skill_name = crate::skills::add_library_skill(&user_home, &request.kind)
        .map_err(skill_install_error_response)?;
    Ok(Json(json!({
        "ok": true,
        "skill_name": skill_name,
        "library": "user",
    })))
}

pub async fn remove_skill_from_catalog(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::RemoveSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    crate::skills::remove_library_skill(&user_home, &request.name).map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "skill_name": request.name,
        "library": "user",
    })))
}

pub async fn update_skill_catalog(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::UpdateSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let result = crate::skills::update_library_skills(&user_home, request.name.as_deref())
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "library": "user",
        "result": result,
    })))
}

pub async fn check_skill_catalog(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::CheckSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let result = crate::skills::check_library_skills(&user_home, request.name.as_deref())
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "library": "user",
        "result": result,
    })))
}

pub async fn enable_skill(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::EnableSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let skill_name = crate::skills::enable_agent_skill(
        &agent_home,
        Some(&user_home),
        &request.name,
        &request.mode,
    )
    .map_err(skill_install_error_response)?;
    runtime
        .append_audit_event(
            "skill_enabled",
            json!({
                "target_agent_id": agent_id,
                "skill_name": skill_name,
                "mode": request.mode,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skill_name": skill_name,
    })))
}

pub async fn uninstall_skill(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::UninstallSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    crate::skills::uninstall_skill(&agent_home, &request.name).map_err(error_response)?;
    runtime
        .append_audit_event(
            "skill_uninstalled",
            json!({
                "target_agent_id": agent_id,
                "skill_name": request.name,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skill_name": request.name,
        "compat": "uninstall",
    })))
}

pub async fn disable_skill(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::types::DisableSkillRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let runtime = state
        .host
        .get_public_agent(&agent_id)
        .await
        .map_err(agent_access_error)?;
    let agent_home = runtime.agent_home();
    crate::skills::disable_agent_skill(&agent_home, &request.name).map_err(error_response)?;
    runtime
        .append_audit_event(
            "skill_disabled",
            json!({
                "target_agent_id": agent_id,
                "skill_name": request.name,
            }),
        )
        .map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skill_name": request.name,
    })))
}

pub async fn skills_catalog(
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;

    let scope_filter = params.get("scope").and_then(|s| match s.as_str() {
        "agent" => Some(crate::types::SkillScope::Agent),
        "workspace" => Some(crate::types::SkillScope::Workspace),
        "user" => Some(crate::types::SkillScope::User),
        _ => None,
    });

    let user_home = crate::agent_template::user_home_dir().ok();
    let roots = crate::skills::existing_skill_roots(
        user_home.as_deref(),
        &crate::skills::COMPAT_SKILL_ROOT_SUFFIXES,
    )
    .into_iter()
    .map(|root| {
        crate::skills::skill_root_registration(
            crate::types::SkillRootSourceKind::UserGlobal,
            None,
            root,
        )
    })
    .collect::<Vec<_>>();

    let mut registry = state.skills_registry.write().await;
    registry
        .sync_effective_roots(roots.clone())
        .map_err(error_response)?;
    let catalog = registry.catalog_for_roots(&roots, scope_filter);
    Ok(Json(json!({
        "ok": true,
        "library": "user",
        "catalog": catalog,
        "scope": scope_filter,
    })))
}
