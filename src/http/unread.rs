use super::*;

use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct MarkBriefReadRequest {
    pub(crate) read_through_event_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oidc_read_scope_is_isolated_per_principal() {
        let scope_a = observer_visibility_scope_for_principal(
            "runtime_fixture",
            "user-a",
            observer_sync::CONTROL_SCOPE_ENTITLEMENT,
            0,
        );
        let scope_b = observer_visibility_scope_for_principal(
            "runtime_fixture",
            "user-b",
            observer_sync::CONTROL_SCOPE_ENTITLEMENT,
            0,
        );

        assert_ne!(scope_a, scope_b);
    }
}

fn principal_and_scope(headers: &HeaderMap, state: &AppState) -> Result<(String, String)> {
    let roster = state.host.agent_roster_snapshot()?;
    let (principal, entitlement) =
        if state.host.config().auth.mode == crate::authentication::AuthenticationMode::Oidc {
            (
                control_actor(headers, state)?.principal_id(),
                observer_sync::CONTROL_SCOPE_ENTITLEMENT,
            )
        } else {
            let (principal, entitlement) = observer_sync::observer_scope_authority(state);
            (principal.to_string(), entitlement)
        };
    let scope = observer_visibility_scope_for_principal(
        &roster.runtime_id,
        &principal,
        entitlement,
        roster.visibility_policy_generation,
    );
    Ok((principal, scope))
}

fn observer_visibility_scope_for_principal(
    runtime_id: &str,
    principal: &str,
    entitlement: &str,
    visibility_policy_generation: u64,
) -> String {
    observer_sync::observer_visibility_scope_for(
        runtime_id,
        principal,
        entitlement,
        visibility_policy_generation,
    )
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
