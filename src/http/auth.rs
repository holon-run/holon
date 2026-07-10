use super::*;

#[derive(Debug, Serialize)]
struct OAuthDeviceStartResponse {
    ok: bool,
    login_id: String,
    verification_url: String,
    user_code: String,
    interval: u64,
    expires_at: chrono::DateTime<Utc>,
    job: jobs::JobSnapshot,
}

pub async fn start_codex_device_login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let device_code = crate::auth::request_codex_device_code()
        .await
        .map_err(error_response)?;
    let job = jobs::create_oauth_device_login_job(
        state,
        crate::auth::OAuthProviderConfig::codex(),
        device_code.clone(),
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(OAuthDeviceStartResponse {
            ok: true,
            login_id: job.id.clone(),
            verification_url: device_code.verification_url,
            user_code: device_code.user_code,
            interval: device_code.interval,
            expires_at: device_code.expires_at,
            job,
        }),
    ))
}

pub async fn start_oauth_device_login(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let config = crate::auth::oauth_provider_config(&provider).ok_or_else(|| {
        error_response(anyhow::anyhow!(
            "provider {provider} does not support OAuth device login"
        ))
    })?;
    let device_code = crate::auth::request_oauth_device_code(&config)
        .await
        .map_err(error_response)?;
    let job = jobs::create_oauth_device_login_job(state, config, device_code.clone());
    Ok((
        StatusCode::ACCEPTED,
        Json(OAuthDeviceStartResponse {
            ok: true,
            login_id: job.id.clone(),
            verification_url: device_code.verification_url,
            user_code: device_code.user_code,
            interval: device_code.interval,
            expires_at: device_code.expires_at,
            job,
        }),
    ))
}
