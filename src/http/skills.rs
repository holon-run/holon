use super::*;
use crate::types::SkillRootRegistration;
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
    let agent_home = runtime.agent_home();
    let skills = crate::skills::list_installed_skills(&agent_home).map_err(error_response)?;
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "skills": skills,
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
    let mut registry = crate::skills::registry::SkillsRegistry::new();

    let root = SkillRootRegistration {
        source_kind: crate::types::SkillRootSourceKind::UserGlobal,
        owner_agent_id: None,
        root_path: user_home.clone().unwrap_or_default().join(".agents/skills"),
        scan_status: crate::types::SkillRootScanStatus::NeverScanned,
        watch_status: crate::types::SkillRootWatchStatus::NotWatched,
    };
    registry.register_root(root).map_err(error_response)?;

    let catalog = registry.catalog_with_filter(scope_filter);
    Ok(Json(json!({
        "ok": true,
        "catalog": catalog,
        "scope": scope_filter,
    })))
}
