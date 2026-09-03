use super::*;
use url::Url;

#[derive(Debug, Deserialize)]
pub struct OidcCallbackQuery {
    pub state: String,
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionExchangeRequest {
    pub credential: String,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    ok: bool,
    expires_at: chrono::DateTime<Utc>,
    user_id: String,
}

fn session_cookie(state: &AppState, credential: &str) -> String {
    let secure = state
        .host
        .config()
        .auth
        .oidc
        .as_ref()
        .and_then(|oidc| oidc.redirect_uri.as_deref())
        .and_then(|redirect_uri| Url::parse(redirect_uri).ok())
        .is_some_and(|redirect_uri| redirect_uri.scheme() == "https");
    let secure_attribute = if secure { "; Secure" } else { "" };
    format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax{}",
        SESSION_COOKIE_NAME, credential, secure_attribute
    )
}

pub async fn start_oidc_login(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let config = state.host.config().auth.clone();
    let client = crate::oidc::OidcClient::new(config).map_err(error_response)?;
    let login = client
        .begin_login(state.host.runtime_db(), Utc::now())
        .await
        .map_err(error_response)?;
    let location = HeaderValue::from_str(&login.authorization_url)
        .map_err(|error| error_response(anyhow!("invalid OIDC authorization URL: {error}")))?;
    Ok((StatusCode::FOUND, [(LOCATION, location)]))
}

pub async fn complete_oidc_login(
    State(state): State<Arc<AppState>>,
    Query(query): Query<OidcCallbackQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let config = state.host.config().auth.clone();
    let client = crate::oidc::OidcClient::new(config).map_err(error_response)?;
    let session = client
        .complete_login(
            state.host.runtime_db(),
            &query.state,
            &query.code,
            Utc::now(),
        )
        .await
        .map_err(error_response)?;
    let cookie = session_cookie(&state, &session.credential);
    Ok((
        StatusCode::FOUND,
        [
            (LOCATION, HeaderValue::from_static("/")),
            (
                SET_COOKIE,
                HeaderValue::from_str(&cookie)
                    .map_err(|error| error_response(anyhow!("invalid session cookie: {error}")))?,
            ),
        ],
    ))
}

pub async fn exchange_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SessionExchangeRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    if request.credential.trim().is_empty() {
        return Err(bad_request("credential must not be empty"));
    }
    let session = crate::oidc::exchange_bootstrap(
        state.host.runtime_db(),
        &state.host.config().auth,
        &request.credential,
        Utc::now(),
    )
    .map_err(error_response)?;
    let cookie = session_cookie(&state, &session.credential);
    Ok((
        StatusCode::OK,
        [(
            SET_COOKIE,
            HeaderValue::from_str(&cookie)
                .map_err(|error| error_response(anyhow!("invalid session cookie: {error}")))?,
        )],
        Json(SessionResponse {
            ok: true,
            expires_at: session.record.expires_at,
            user_id: session.record.user_id,
        }),
    ))
}

pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let session =
        authenticate_session(&headers, &state).map_err(|error| auth_required(error.to_string()))?;
    state
        .host
        .runtime_db()
        .authentication()
        .revoke_session(&session.session_digest, Utc::now())
        .map_err(error_response)?;
    Ok((
        StatusCode::NO_CONTENT,
        [(
            SET_COOKIE,
            HeaderValue::from_static("holon_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax"),
        )],
    ))
}

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
