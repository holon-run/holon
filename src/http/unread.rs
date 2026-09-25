use super::*;

use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct MarkBriefReadRequest {
    pub(crate) read_through_event_seq: u64,
}

fn principal_and_scope(headers: &HeaderMap, state: &AppState) -> Result<(String, String)> {
    let roster = state.host.agent_roster_snapshot()?;
    let (principal, scope) =
        if state.host.config().auth.mode == crate::authentication::AuthenticationMode::Oidc {
            let actor = control_actor(headers, state)?;
            let principal = actor.principal_id();
            let scope = observer_sync::observer_visibility_scope_for(
                &roster.runtime_id,
                &principal,
                observer_sync::CONTROL_SCOPE_ENTITLEMENT,
                roster.visibility_policy_generation,
            );
            (principal, scope)
        } else {
            let (principal, _) = observer_sync::observer_scope_authority(state);
            let scope = observer_sync::observer_visibility_scope(
                state,
                &roster.runtime_id,
                roster.visibility_policy_generation,
            );
            (principal.to_string(), scope)
        };
    Ok((principal, scope))
}

pub(crate) async fn brief_read_states(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AxumResponse {
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return auth_required(error.to_string()).into_response();
    }
    let (principal, scope) = match principal_and_scope(&headers, &state) {
        Ok(value) => value,
        Err(error) => return error_response(error).into_response(),
    };
    match state
        .host
        .runtime_db()
        .brief_read_states(&principal, &scope)
    {
        Ok(states) => Json(states).into_response(),
        Err(error) => error_response(error).into_response(),
    }
}

pub(crate) async fn brief_read_state(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AxumResponse {
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return auth_required(error.to_string()).into_response();
    }
    let (principal, scope) = match principal_and_scope(&headers, &state) {
        Ok(value) => value,
        Err(error) => return error_response(error).into_response(),
    };
    match state
        .host
        .runtime_db()
        .brief_read_state(&principal, &scope, &agent_id)
    {
        Ok(Some(state)) => Json(state).into_response(),
        Ok(None) => http_error(
            StatusCode::NOT_FOUND,
            HttpErrorEnvelope::new(
                "agent_not_found",
                format!("public active agent {agent_id} was not found"),
            ),
        )
        .into_response(),
        Err(error) => error_response(error).into_response(),
    }
}

pub(crate) async fn mark_brief_read(
    Path(agent_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<MarkBriefReadRequest>,
) -> AxumResponse {
    if let Err(error) = authorize_remote_access(&headers, &state) {
        return auth_required(error.to_string()).into_response();
    }
    let (principal, scope) = match principal_and_scope(&headers, &state) {
        Ok(value) => value,
        Err(error) => return error_response(error).into_response(),
    };
    match state.host.runtime_db().mark_brief_read(
        &principal,
        &scope,
        &agent_id,
        request.read_through_event_seq,
    ) {
        Ok(Some(result)) => Json(result).into_response(),
        Ok(None) => http_error(
            StatusCode::NOT_FOUND,
            HttpErrorEnvelope::new(
                "agent_not_found",
                format!("public active agent {agent_id} was not found"),
            ),
        )
        .into_response(),
        Err(error) => error_response(error).into_response(),
    }
}
