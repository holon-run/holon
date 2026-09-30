use super::*;
use std::collections::HashMap;
use url::Url;

const PAIRING_TTL: chrono::Duration = chrono::Duration::minutes(2);
const MAX_PAIRING_TICKETS: usize = 128;

#[derive(Default)]
pub(crate) struct PairingTickets {
    // Only digests are kept in memory; a restart invalidates outstanding tickets.
    expires: HashMap<String, chrono::DateTime<Utc>>,
}

fn oidc_state_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                (name == "holon_oidc_state").then_some(value)
            })
        })
}

fn issue_local_session(
    state: &AppState,
    auth_method: &str,
    now: chrono::DateTime<Utc>,
) -> Result<crate::oidc::IssuedSession, (StatusCode, Json<Value>)> {
    let user_id = "local-static-token";
    state
        .host
        .runtime_db()
        .authentication()
        .upsert_user(&crate::authentication::AuthUserRecord {
            user_id: user_id.to_string(),
            issuer: "local".to_string(),
            subject: "static-token".to_string(),
            display_name: Some("Local token user".to_string()),
            email: None,
            created_at: now,
            updated_at: now,
            disabled_at: None,
        })
        .map_err(error_response)?;
    crate::oidc::issue_session(
        state.host.runtime_db(),
        &state.host.config().auth,
        user_id,
        auth_method,
        now,
    )
    .map_err(error_response)
}

impl PairingTickets {
    fn issue(&mut self, now: chrono::DateTime<Utc>) -> Option<(String, chrono::DateTime<Utc>)> {
        self.expires.retain(|_, expiry| *expiry > now);
        if self.expires.len() >= MAX_PAIRING_TICKETS {
            return None;
        }
        let ticket = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expires_at = now + PAIRING_TTL;
        self.expires
            .insert(crate::authentication::digest_secret(&ticket), expires_at);
        Some((ticket, expires_at))
    }

    fn consume(&mut self, ticket: &str, now: chrono::DateTime<Utc>) -> bool {
        self.expires
            .remove(&crate::authentication::digest_secret(ticket))
            .is_some_and(|expiry| expiry > now)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PairingRedeemRequest {
    ticket: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PairingIssueResponse {
    ticket: String,
    expires_at: chrono::DateTime<Utc>,
}

pub async fn issue_pairing_ticket(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    if state.host.config().auth.mode != crate::authentication::AuthenticationMode::Local {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "pairing requires local authentication"})),
        ));
    }
    let has_valid_token = state
        .host
        .config()
        .control_token
        .as_deref()
        .is_some_and(|expected| {
            headers
                .get(AUTHORIZATION)
                .and_then(|header| header.to_str().ok())
                .and_then(|header| header.strip_prefix("Bearer "))
                == Some(expected)
        });
    if !state.uses_trusted_local_admission()
        && !has_valid_token
        && authenticate_session(&headers, &state).is_err()
    {
        return Err(auth_required("authentication required"));
    }
    let (ticket, expires_at) = state
        .pairing_tickets
        .lock()
        .map_err(|_| error_response(anyhow!("pairing ticket store unavailable")))?
        .issue(Utc::now())
        .ok_or_else(|| {
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": "too many active pairing tickets"})),
            )
        })?;
    Ok((
        [(CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(PairingIssueResponse { ticket, expires_at }),
    ))
}

async fn redeem_ticket(
    state: &AppState,
    ticket: &str,
) -> Result<(crate::oidc::IssuedSession, String), (StatusCode, Json<Value>)> {
    if state.host.config().auth.mode != crate::authentication::AuthenticationMode::Local {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "pairing requires local authentication"})),
        ));
    }
    if ticket.len() != 64 || !ticket.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(auth_required("invalid or expired pairing ticket"));
    }
    let consumed = state
        .pairing_tickets
        .lock()
        .map_err(|_| error_response(anyhow!("pairing ticket store unavailable")))?
        .consume(ticket, Utc::now());
    if !consumed {
        return Err(auth_required("invalid or expired pairing ticket"));
    }
    let session = issue_local_session(state, "pairing_ticket", Utc::now())?;
    let cookie = session_cookie(state, &session.credential);
    Ok((session, cookie))
}

pub async fn redeem_pairing_ticket(
    State(state): State<Arc<AppState>>,
    ApiJson(request): ApiJson<PairingRedeemRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let (session, cookie) = redeem_ticket(&state, &request.ticket).await?;
    Ok((
        [
            (
                SET_COOKIE,
                HeaderValue::from_str(&cookie).map_err(|error| error_response(anyhow!(error)))?,
            ),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        Json(SessionResponse {
            ok: true,
            expires_at: session.record.expires_at,
            user_id: session.record.user_id,
        }),
    ))
}

pub async fn redeem_pairing_ticket_native(
    State(state): State<Arc<AppState>>,
    ApiJson(request): ApiJson<PairingRedeemRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let (session, cookie) = redeem_ticket(&state, &request.ticket).await?;
    Ok((
        [
            (
                SET_COOKIE,
                HeaderValue::from_str(&cookie).map_err(|error| error_response(anyhow!(error)))?,
            ),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        Json(NativeSessionResponse {
            credential: session.credential,
            ok: true,
            expires_at: session.record.expires_at,
            user_id: session.record.user_id,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct OidcCallbackQuery {
    pub state: String,
    pub code: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionExchangeRequest {
    pub credential: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SessionResponse {
    ok: bool,
    expires_at: Option<chrono::DateTime<Utc>>,
    user_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NativeSessionResponse {
    credential: String,
    ok: bool,
    expires_at: Option<chrono::DateTime<Utc>>,
    user_id: String,
}

#[derive(Debug, Serialize)]
pub struct AuthMethodResponse {
    mode: crate::authentication::AuthenticationMode,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CurrentUserResponse {
    ok: bool,
    user_id: String,
    display_name: Option<String>,
    auth_method: String,
}

pub async fn session_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let actor =
        control_actor(&headers, &state).map_err(|error| auth_required(error.to_string()))?;
    let (user_id, display_name, auth_method) = match &actor {
        ControlActor::User {
            user_id,
            auth_method,
            ..
        } => (
            user_id.clone(),
            actor.display_name_or_fallback(),
            auth_method.clone(),
        ),
        ControlActor::LocalControl => ("control".to_string(), None, "local_control".to_string()),
    };
    Ok(Json(CurrentUserResponse {
        ok: true,
        user_id,
        display_name,
        auth_method,
    }))
}

pub async fn auth_method(State(state): State<Arc<AppState>>) -> Json<AuthMethodResponse> {
    Json(AuthMethodResponse {
        mode: state.host.config().auth.mode,
    })
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
    let state_cookie = HeaderValue::from_str(&format!(
        "holon_oidc_state={}; Path=/; HttpOnly; SameSite=Lax",
        login.state
    ))
    .map_err(|error| error_response(anyhow!("invalid OIDC state cookie: {error}")))?;
    Ok((
        StatusCode::FOUND,
        [(LOCATION, location), (SET_COOKIE, state_cookie)],
    ))
}

pub async fn complete_oidc_login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<OidcCallbackQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let browser_state = oidc_state_cookie(&headers);
    if browser_state != Some(query.state.as_str()) {
        return Err(error_response(anyhow!(
            "OIDC login browser session does not match"
        )));
    }
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
            (
                SET_COOKIE,
                HeaderValue::from_static(
                    "holon_oidc_state=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
                ),
            ),
        ],
    ))
}

async fn exchange_session_credential(
    State(state): State<Arc<AppState>>,
    ApiJson(request): ApiJson<SessionExchangeRequest>,
) -> Result<(crate::oidc::IssuedSession, String), (StatusCode, Json<Value>)> {
    if request.credential.trim().is_empty() {
        return Err(bad_request("credential must not be empty"));
    }
    let config = state.host.config();
    let now = Utc::now();
    let session = if config.auth.mode == crate::authentication::AuthenticationMode::Local {
        let expected = config
            .control_token
            .as_deref()
            .ok_or_else(|| bad_request("static token authentication is not configured"))?;
        if request.credential != expected {
            return Err(auth_required("invalid static token"));
        }
        issue_local_session(&state, "static_token", now)?
    } else {
        crate::oidc::exchange_bootstrap(
            state.host.runtime_db(),
            &config.auth,
            &request.credential,
            now,
        )
        .map_err(error_response)?
    };
    let cookie = session_cookie(&state, &session.credential);
    Ok((session, cookie))
}

pub async fn exchange_session(
    State(state): State<Arc<AppState>>,
    ApiJson(request): ApiJson<SessionExchangeRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let (session, cookie) = exchange_session_credential(State(state), ApiJson(request)).await?;
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

pub async fn exchange_session_native(
    State(state): State<Arc<AppState>>,
    ApiJson(request): ApiJson<SessionExchangeRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let (session, cookie) = exchange_session_credential(State(state), ApiJson(request)).await?;
    Ok((
        StatusCode::OK,
        [(
            SET_COOKIE,
            HeaderValue::from_str(&cookie)
                .map_err(|error| error_response(anyhow!("invalid session cookie: {error}")))?,
        )],
        Json(NativeSessionResponse {
            credential: session.credential,
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

#[cfg(test)]
mod pairing_tests {
    use super::*;

    #[test]
    fn ticket_is_single_use_expires_and_is_bounded() {
        let mut tickets = PairingTickets::default();
        let now = Utc::now();
        let (ticket, expiry) = tickets.issue(now).unwrap();
        assert_eq!(expiry, now + PAIRING_TTL);
        assert!(!tickets.expires.contains_key(&ticket));
        assert!(tickets.consume(&ticket, now));
        assert!(!tickets.consume(&ticket, now));

        let (expired, _) = tickets.issue(now).unwrap();
        assert!(!tickets.consume(&expired, now + PAIRING_TTL));
        for _ in 0..MAX_PAIRING_TICKETS {
            assert!(tickets.issue(now).is_some());
        }
        assert!(tickets.issue(now).is_none());
        assert!(tickets.issue(now + PAIRING_TTL).is_some());
    }

    #[test]
    fn concurrent_redemption_has_only_one_winner() {
        let now = Utc::now();
        let mut tickets = PairingTickets::default();
        let (ticket, _) = tickets.issue(now).unwrap();
        let tickets = std::sync::Arc::new(std::sync::Mutex::new(tickets));
        let results = std::thread::scope(|scope| {
            let left = {
                let tickets = tickets.clone();
                let ticket = ticket.clone();
                scope.spawn(move || tickets.lock().unwrap().consume(&ticket, now))
            };
            let right = {
                let tickets = tickets.clone();
                scope.spawn(move || tickets.lock().unwrap().consume(&ticket, now))
            };
            (left.join().unwrap(), right.join().unwrap())
        });
        assert_ne!(results.0, results.1);
    }
}

#[cfg(test)]
mod oidc_tests {
    use super::*;

    #[test]
    fn extracts_browser_state_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            "other=value; holon_oidc_state=opaque-state"
                .parse()
                .unwrap(),
        );
        assert_eq!(oidc_state_cookie(&headers), Some("opaque-state"));
    }

    #[test]
    fn missing_browser_state_is_rejected() {
        assert_eq!(oidc_state_cookie(&HeaderMap::new()), None);
    }
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
