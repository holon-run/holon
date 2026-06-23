use super::*;
use crate::types::{
    SkillRootRegistration, SkillRootScanStatus, SkillRootSourceKind, SkillRootWatchStatus,
};
use axum::extract::Query;
use std::path::PathBuf;

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

    let agent_id = params.get("agent_id").cloned();
    let mut roots = Vec::new();
    if let Some(user_home) = crate::agent_template::user_home_dir().ok() {
        for root in crate::skills::existing_skill_roots(
            Some(&user_home),
            &crate::skills::COMPAT_SKILL_ROOT_SUFFIXES,
        ) {
            roots.push(skill_root_registration(
                SkillRootSourceKind::UserGlobal,
                None,
                root,
            ));
        }
    }

    if let Some(agent_id) = agent_id.as_deref() {
        let runtime = state
            .host
            .get_public_agent(agent_id)
            .await
            .map_err(agent_access_error)?;
        let agent_home = runtime.agent_home();
        for root in crate::skills::existing_skill_roots(
            Some(&agent_home),
            &crate::skills::SKILL_ROOT_SUFFIXES,
        ) {
            roots.push(skill_root_registration(
                SkillRootSourceKind::AgentHome,
                Some(agent_id.to_string()),
                root,
            ));
        }
        let execution = runtime.execution_snapshot().await.map_err(error_response)?;
        for root in crate::skills::existing_skill_roots(
            Some(&execution.workspace_anchor),
            &crate::skills::COMPAT_SKILL_ROOT_SUFFIXES,
        ) {
            roots.push(skill_root_registration(
                SkillRootSourceKind::Workspace,
                None,
                root,
            ));
        }
    }

    let mut registry = state.skills_registry.write().await;
    for root in roots {
        registry.register_root(root).map_err(error_response)?;
    }
    registry.rescan();
    let catalog = registry.catalog_with_filter(scope_filter);
    Ok(Json(json!({
        "ok": true,
        "agent_id": agent_id,
        "catalog": catalog,
        "scope": scope_filter,
    })))
}

fn skill_root_registration(
    source_kind: SkillRootSourceKind,
    owner_agent_id: Option<String>,
    root_path: PathBuf,
) -> SkillRootRegistration {
    SkillRootRegistration {
        source_kind,
        owner_agent_id,
        root_path,
        scan_status: SkillRootScanStatus::NeverScanned,
        watch_status: SkillRootWatchStatus::NotWatched,
    }
}
