use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct CodexCliCredential {
    pub access_token: String,
    pub account_id: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AuthDotJson {
    #[serde(default)]
    tokens: Option<TokenData>,
    #[serde(default)]
    last_refresh: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TokenData {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    id_token: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct RefreshedCodexOAuthProfile {
    pub material: String,
    pub credential: CodexCliCredential,
}

#[derive(Debug, Clone)]
pub struct CodexOAuthLoginResult {
    pub material: String,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexDeviceCode {
    pub verification_url: String,
    pub user_code: String,
    pub interval: u64,
    pub expires_at: DateTime<Utc>,
    #[serde(skip)]
    device_auth_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexOAuthRefreshFailureKind {
    Expired,
    Reused,
    Revoked,
    Other,
}

impl CodexOAuthRefreshFailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Expired => "refresh_token_expired",
            Self::Reused => "refresh_token_reused",
            Self::Revoked => "refresh_token_revoked",
            Self::Other => "refresh_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodexOAuthRefreshFailure {
    pub kind: CodexOAuthRefreshFailureKind,
    pub message: String,
}

pub fn load_codex_cli_credential(codex_home: &Path) -> Result<CodexCliCredential> {
    let home = canonical_or_original(codex_home);
    let mut candidates = Vec::new();
    if let Ok(auth) = load_auth_from_file(&home) {
        candidates.push(auth_to_credential(auth, "file")?);
    }
    if cfg!(target_os = "macos") {
        if let Ok(auth) = load_auth_from_macos_keychain(&home) {
            candidates.push(auth_to_credential(auth, "keychain")?);
        }
    }
    candidates
        .into_iter()
        .max_by_key(codex_cli_credential_freshness_key)
        .ok_or_else(|| {
            anyhow!(
                "no Codex CLI credential found in {}; run `codex login` to configure the external fallback",
                home.display()
            )
        })
}

pub fn load_codex_oauth_profile_credential(
    material: &str,
    profile: &str,
) -> Result<CodexCliCredential> {
    let auth: AuthDotJson = serde_json::from_str(material)
        .with_context(|| format!("failed to parse Holon credential profile {profile}"))?;
    auth_to_credential(auth, &format!("credential_profile:{profile}"))
}

pub async fn refresh_codex_oauth_profile_material(
    client: &reqwest::Client,
    material: &str,
    profile: &str,
) -> std::result::Result<RefreshedCodexOAuthProfile, CodexOAuthRefreshFailure> {
    let mut auth: AuthDotJson =
        serde_json::from_str(material).map_err(|error| CodexOAuthRefreshFailure {
            kind: CodexOAuthRefreshFailureKind::Other,
            message: format!("failed to parse Holon credential profile {profile}: {error}"),
        })?;
    let refresh_token = auth
        .tokens
        .as_ref()
        .map(|tokens| tokens.refresh_token.clone())
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| CodexOAuthRefreshFailure {
            kind: CodexOAuthRefreshFailureKind::Other,
            message: format!("Holon credential profile {profile} does not contain a refresh token"),
        })?;

    let refreshed = request_codex_oauth_refresh(client, refresh_token).await?;
    let tokens = auth.tokens.get_or_insert_with(|| TokenData {
        access_token: String::new(),
        refresh_token: String::new(),
        account_id: None,
        id_token: None,
    });
    if let Some(id_token) = refreshed.id_token {
        tokens.id_token = Some(Value::String(id_token));
    }
    tokens.access_token = refreshed
        .access_token
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| CodexOAuthRefreshFailure {
            kind: CodexOAuthRefreshFailureKind::Other,
            message: "OpenAI Codex OAuth refresh response did not include an access token"
                .to_string(),
        })?;
    if let Some(refresh_token) = refreshed.refresh_token {
        tokens.refresh_token = refresh_token;
    }
    auth.last_refresh = Some(Utc::now());
    let credential = auth_to_credential(auth.clone(), &format!("credential_profile:{profile}"))
        .map_err(|error| CodexOAuthRefreshFailure {
            kind: CodexOAuthRefreshFailureKind::Other,
            message: format!("refreshed Holon credential profile {profile} is invalid: {error}"),
        })?;
    let material = serde_json::to_string(&auth).map_err(|error| CodexOAuthRefreshFailure {
        kind: CodexOAuthRefreshFailureKind::Other,
        message: format!(
            "failed to serialize refreshed Holon credential profile {profile}: {error}"
        ),
    })?;
    Ok(RefreshedCodexOAuthProfile {
        material,
        credential,
    })
}

#[derive(Debug, Serialize)]
struct RefreshRequest {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_ISSUER: &str = "https://auth.openai.com";
const CODEX_OAUTH_ISSUER_OVERRIDE_ENV_VAR: &str = "CODEX_OAUTH_ISSUER_OVERRIDE";
const CODEX_REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
const CODEX_OAUTH_CALLBACK_PORT: u16 = 1455;
const CODEX_OAUTH_CALLBACK_FALLBACK_PORT: u16 = 1457;
const CODEX_DEVICE_AUTH_MAX_WAIT_SECONDS: i64 = 15 * 60;

pub fn run_codex_oauth_login_profile_material() -> Result<CodexOAuthLoginResult> {
    run_codex_oauth_login_profile_material_with_browser(true)
}

pub async fn request_codex_device_code() -> Result<CodexDeviceCode> {
    let issuer = std::env::var(CODEX_OAUTH_ISSUER_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| CODEX_OAUTH_ISSUER.to_string());
    let base_url = issuer.trim_end_matches('/');
    let response = reqwest::Client::new()
        .post(format!("{base_url}/api/accounts/deviceauth/usercode"))
        .header("Content-Type", "application/json")
        .json(&DeviceUserCodeRequest {
            client_id: CODEX_OAUTH_CLIENT_ID,
        })
        .send()
        .await
        .context("failed to request OpenAI Codex device code")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "OpenAI Codex device code request failed with HTTP {status}: {}",
            try_parse_refresh_error_message(&body).unwrap_or(body)
        );
    }
    let body = response
        .json::<DeviceUserCodeResponse>()
        .await
        .context("failed to parse OpenAI Codex device code response")?;
    let expires_in = body
        .expires_in
        .unwrap_or(CODEX_DEVICE_AUTH_MAX_WAIT_SECONDS)
        .clamp(1, CODEX_DEVICE_AUTH_MAX_WAIT_SECONDS);
    Ok(CodexDeviceCode {
        verification_url: format!("{base_url}/codex/device"),
        user_code: body.user_code,
        interval: body.interval,
        expires_at: Utc::now() + chrono::Duration::seconds(expires_in),
        device_auth_id: body.device_auth_id,
    })
}

pub async fn complete_codex_device_login(
    device_code: CodexDeviceCode,
) -> Result<CodexOAuthLoginResult> {
    let issuer = std::env::var(CODEX_OAUTH_ISSUER_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| CODEX_OAUTH_ISSUER.to_string());
    let base_url = issuer.trim_end_matches('/');
    let code_response = poll_codex_device_code_for_authorization(base_url, &device_code).await?;
    let redirect_uri = format!("{base_url}/deviceauth/callback");
    let tokens = exchange_codex_oauth_code_for_tokens(
        base_url,
        &redirect_uri,
        &code_response.code_verifier,
        &code_response.authorization_code,
    )
    .await?;
    codex_oauth_tokens_to_login_result(tokens)
}

fn run_codex_oauth_login_profile_material_with_browser(
    open_browser: bool,
) -> Result<CodexOAuthLoginResult> {
    let issuer = std::env::var(CODEX_OAUTH_ISSUER_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| CODEX_OAUTH_ISSUER.to_string());
    let listener = bind_codex_oauth_callback_listener()?;
    listener
        .set_nonblocking(false)
        .context("failed to configure Codex OAuth callback listener")?;
    let port = listener
        .local_addr()
        .context("failed to read Codex OAuth callback listener address")?
        .port();
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let pkce = generate_codex_pkce();
    let state = generate_oauth_random_string();
    let auth_url = build_codex_oauth_authorize_url(&issuer, &redirect_uri, &pkce, &state);

    if open_browser {
        open_url_in_browser(&auth_url);
    }
    eprintln!(
        "Starting Holon OpenAI Codex OAuth login on http://localhost:{port}.\nIf your browser did not open, navigate to this URL:\n\n{auth_url}\n"
    );

    loop {
        let (mut stream, _) = listener
            .accept()
            .context("failed to receive Codex OAuth callback")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .context("failed to configure Codex OAuth callback read timeout")?;
        let mut buffer = [0_u8; 8192];
        let size = stream
            .read(&mut buffer)
            .context("failed to read Codex OAuth callback request")?;
        let request = String::from_utf8_lossy(&buffer[..size]);
        let Some(target) = parse_http_get_target(&request) else {
            write_http_response(&mut stream, 400, "Bad Request", "Bad Request")?;
            continue;
        };

        match handle_codex_oauth_callback(
            target,
            &issuer,
            &redirect_uri,
            &pkce.code_verifier,
            &state,
        ) {
            Ok(Some(result)) => {
                write_http_response(
                    &mut stream,
                    200,
                    "OK",
                    "Holon OpenAI Codex login complete. You can close this tab and return to Holon.",
                )?;
                return Ok(result);
            }
            Ok(None) => {
                write_http_response(&mut stream, 404, "Not Found", "Not Found")?;
            }
            Err(error) => {
                let body = format!("Holon OpenAI Codex login failed: {error}");
                write_http_response(&mut stream, 400, "Bad Request", &body)?;
                return Err(error);
            }
        }
    }
}

async fn request_codex_oauth_refresh(
    client: &reqwest::Client,
    refresh_token: String,
) -> std::result::Result<RefreshResponse, CodexOAuthRefreshFailure> {
    let endpoint = std::env::var(CODEX_REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| CODEX_REFRESH_TOKEN_URL.to_string());
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .json(&RefreshRequest {
            client_id: CODEX_OAUTH_CLIENT_ID,
            grant_type: "refresh_token",
            refresh_token,
        })
        .send()
        .await
        .map_err(|error| CodexOAuthRefreshFailure {
            kind: CodexOAuthRefreshFailureKind::Other,
            message: format!("failed to refresh OpenAI Codex OAuth token: {error}"),
        })?;
    let status = response.status();
    if status.is_success() {
        return response.json::<RefreshResponse>().await.map_err(|error| {
            CodexOAuthRefreshFailure {
                kind: CodexOAuthRefreshFailureKind::Other,
                message: format!("failed to parse OpenAI Codex OAuth refresh response: {error}"),
            }
        });
    }

    let body = response.text().await.unwrap_or_default();
    let kind = classify_refresh_failure_kind(&body);
    let message = match kind {
        CodexOAuthRefreshFailureKind::Expired => {
            "OpenAI Codex refresh token expired; run Holon onboarding login for openai-codex again.".to_string()
        }
        CodexOAuthRefreshFailureKind::Reused => {
            "OpenAI Codex refresh token was reused; run Holon onboarding login for openai-codex again.".to_string()
        }
        CodexOAuthRefreshFailureKind::Revoked => {
            "OpenAI Codex refresh token was revoked; run Holon onboarding login for openai-codex again.".to_string()
        }
        CodexOAuthRefreshFailureKind::Other => {
            let hint = try_parse_refresh_error_message(&body).unwrap_or_else(|| body.clone());
            format!("OpenAI Codex OAuth refresh failed with HTTP {status}: {hint}")
        }
    };
    Err(CodexOAuthRefreshFailure { kind, message })
}

#[derive(Debug, Clone)]
struct CodexPkce {
    code_verifier: String,
    code_challenge: String,
}

#[derive(Debug, Deserialize)]
struct LoginTokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Serialize)]
struct DeviceUserCodeRequest {
    client_id: &'static str,
}

#[derive(Debug, Deserialize)]
struct DeviceUserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default, deserialize_with = "deserialize_device_interval")]
    interval: u64,
    #[serde(default, deserialize_with = "deserialize_optional_device_seconds")]
    expires_in: Option<i64>,
}

#[derive(Debug, Serialize)]
struct DeviceTokenPollRequest<'a> {
    device_auth_id: &'a str,
    user_code: &'a str,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthorizationResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, PartialEq, Eq)]
enum DevicePollStatus {
    Pending,
    SlowDown,
    Expired,
    AccessDenied,
    Failed(String),
}

fn deserialize_device_interval<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| D::Error::custom("device interval must be a positive integer")),
        Value::String(value) => value.trim().parse::<u64>().map_err(D::Error::custom),
        _ => Err(D::Error::custom(
            "device interval must be a string or integer",
        )),
    }
}

fn deserialize_optional_device_seconds<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| D::Error::custom("device seconds must be a positive integer")),
        Some(Value::String(value)) => value
            .trim()
            .parse::<i64>()
            .map(Some)
            .map_err(D::Error::custom),
        Some(_) => Err(D::Error::custom(
            "device seconds must be a string or integer",
        )),
    }
}

async fn poll_codex_device_code_for_authorization(
    base_url: &str,
    device_code: &CodexDeviceCode,
) -> Result<DeviceAuthorizationResponse> {
    let endpoint = format!("{base_url}/api/accounts/deviceauth/token");
    let started_at = Utc::now();
    let max_wait = chrono::Duration::seconds(CODEX_DEVICE_AUTH_MAX_WAIT_SECONDS);
    let deadline = device_code.expires_at.min(started_at + max_wait);
    let mut interval = device_code.interval.max(1);
    let client = reqwest::Client::new();
    loop {
        let response = client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .json(&DeviceTokenPollRequest {
                device_auth_id: &device_code.device_auth_id,
                user_code: &device_code.user_code,
            })
            .send()
            .await
            .context("failed to poll OpenAI Codex device login")?;
        let status = response.status();
        if status.is_success() {
            return response
                .json::<DeviceAuthorizationResponse>()
                .await
                .context("failed to parse OpenAI Codex device authorization response");
        }
        let body = response.text().await.unwrap_or_default();
        match classify_device_poll_status(status, &body) {
            DevicePollStatus::Pending => {}
            DevicePollStatus::SlowDown => {
                interval = interval.saturating_add(5);
            }
            DevicePollStatus::Expired => {
                anyhow::bail!("OpenAI Codex device login expired before authorization");
            }
            DevicePollStatus::AccessDenied => {
                anyhow::bail!("OpenAI Codex device login was denied by the user");
            }
            DevicePollStatus::Failed(message) => {
                anyhow::bail!("OpenAI Codex device login failed with HTTP {status}: {message}");
            }
        }
        if Utc::now() >= deadline {
            anyhow::bail!("OpenAI Codex device login expired after 15 minutes");
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}

fn bind_codex_oauth_callback_listener() -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", CODEX_OAUTH_CALLBACK_PORT))
        .or_else(|_| TcpListener::bind(("127.0.0.1", CODEX_OAUTH_CALLBACK_FALLBACK_PORT)))
        .context("failed to bind OpenAI Codex OAuth callback listener")
}

fn generate_codex_pkce() -> CodexPkce {
    let code_verifier = generate_oauth_random_string();
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    CodexPkce {
        code_verifier,
        code_challenge,
    }
}

fn generate_oauth_random_string() -> String {
    let mut bytes = Vec::with_capacity(64);
    for _ in 0..4 {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn build_codex_oauth_authorize_url(
    issuer: &str,
    redirect_uri: &str,
    pkce: &CodexPkce,
    state: &str,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", CODEX_OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair(
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        )
        .append_pair("code_challenge", &pkce.code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", "codex_cli_rs")
        .finish();
    format!("{}/oauth/authorize?{query}", issuer.trim_end_matches('/'))
}

fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![url]);
    #[cfg(target_os = "linux")]
    let command = ("xdg-open", vec![url]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", "", url]);

    #[allow(unused_variables)]
    let _ = Command::new(command.0).args(command.1).status();
}

fn parse_http_get_target(request: &str) -> Option<&str> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some("GET"), Some(target), Some(_version)) => Some(target),
        _ => None,
    }
}

fn handle_codex_oauth_callback(
    target: &str,
    issuer: &str,
    redirect_uri: &str,
    code_verifier: &str,
    state: &str,
) -> Result<Option<CodexOAuthLoginResult>> {
    let parsed = url::Url::parse(&format!("http://localhost{target}"))
        .context("failed to parse OpenAI Codex OAuth callback URL")?;
    if parsed.path() != "/auth/callback" {
        return Ok(None);
    }
    let params = parsed
        .query_pairs()
        .into_owned()
        .collect::<std::collections::BTreeMap<_, _>>();
    if params.get("state").map(String::as_str) != Some(state) {
        anyhow::bail!("OpenAI Codex OAuth callback state did not match");
    }
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("no description");
        anyhow::bail!("OpenAI Codex OAuth callback returned {error}: {description}");
    }
    let code = params
        .get("code")
        .filter(|code| !code.trim().is_empty())
        .context("OpenAI Codex OAuth callback did not include an authorization code")?;

    let exchange = exchange_codex_oauth_code_for_tokens(issuer, redirect_uri, code_verifier, code);
    let tokens = if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(exchange))
    } else {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to start OpenAI Codex OAuth token exchange runtime")?;
        runtime.block_on(exchange)
    }?;
    Ok(Some(codex_oauth_tokens_to_login_result(tokens)?))
}

async fn exchange_codex_oauth_code_for_tokens(
    issuer: &str,
    redirect_uri: &str,
    code_verifier: &str,
    code: &str,
) -> Result<LoginTokenResponse> {
    let endpoint = format!("{}/oauth/token", issuer.trim_end_matches('/'));
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("client_id", CODEX_OAUTH_CLIENT_ID)
        .append_pair("code_verifier", code_verifier)
        .finish();
    let response = reqwest::Client::new()
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("failed to exchange OpenAI Codex OAuth code for tokens")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "OpenAI Codex OAuth token exchange failed with HTTP {status}: {}",
            try_parse_refresh_error_message(&body).unwrap_or(body)
        );
    }
    response
        .json::<LoginTokenResponse>()
        .await
        .context("failed to parse OpenAI Codex OAuth token response")
}

fn codex_oauth_tokens_to_login_result(tokens: LoginTokenResponse) -> Result<CodexOAuthLoginResult> {
    let account_id = extract_account_id_from_access_token(&tokens.access_token)
        .or_else(|| extract_account_id_from_id_token(&Value::String(tokens.id_token.clone())));
    let auth = AuthDotJson {
        tokens: Some(TokenData {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            account_id: account_id.clone(),
            id_token: Some(Value::String(tokens.id_token)),
        }),
        last_refresh: Some(Utc::now()),
    };
    let material = serde_json::to_string(&auth)
        .context("failed to serialize OpenAI Codex OAuth credential")?;
    Ok(CodexOAuthLoginResult {
        material,
        account_id,
    })
}

fn write_http_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    reason: &str,
    body: &str,
) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .context("failed to write OpenAI Codex OAuth callback response")?;
    stream
        .flush()
        .context("failed to flush OpenAI Codex OAuth callback response")
}

pub fn codex_cli_auth_file_exists(codex_home: &Path) -> bool {
    let home = canonical_or_original(codex_home);
    auth_file_path(&home).is_file()
        || (cfg!(target_os = "macos") && load_auth_from_macos_keychain(&home).is_ok())
}

fn load_auth_from_file(codex_home: &Path) -> Result<AuthDotJson> {
    let path = auth_file_path(codex_home);
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

fn load_auth_from_macos_keychain(codex_home: &Path) -> Result<AuthDotJson> {
    let account = compute_codex_keychain_account(codex_home);
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Codex Auth",
            "-a",
            &account,
            "-w",
        ])
        .output()
        .context("failed to invoke macOS security tool")?;
    if !output.status.success() {
        return Err(anyhow!(
            "macOS keychain does not contain a Codex Auth entry for account {account}"
        ));
    }
    let secret = String::from_utf8(output.stdout).context("Codex keychain entry was not UTF-8")?;
    serde_json::from_str(secret.trim()).context("failed to parse Codex keychain auth record")
}

fn auth_to_credential(auth: AuthDotJson, source: &str) -> Result<CodexCliCredential> {
    let tokens = auth
        .tokens
        .ok_or_else(|| anyhow!("Codex auth record did not contain OAuth tokens"))?;
    let account_id = tokens
        .account_id
        .or_else(|| {
            tokens
                .id_token
                .as_ref()
                .and_then(extract_account_id_from_id_token)
        })
        .or_else(|| extract_account_id_from_access_token(&tokens.access_token))
        .ok_or_else(|| anyhow!("failed to resolve Codex account id from stored credentials"))?;
    let expires_at = parse_jwt_expiration(&tokens.access_token)
        .ok()
        .flatten()
        .or_else(|| auth.last_refresh.map(|ts| ts + chrono::Duration::hours(1)));

    Ok(CodexCliCredential {
        access_token: tokens.access_token,
        account_id,
        expires_at,
        refreshed_at: auth.last_refresh,
        source: source.to_string(),
    })
}

fn codex_cli_credential_freshness_key(
    credential: &CodexCliCredential,
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>, u8) {
    let source_rank = match credential.source.as_str() {
        "keychain" => 2,
        "file" => 1,
        _ => 0,
    };
    (credential.expires_at, credential.refreshed_at, source_rank)
}

fn auth_file_path(codex_home: &Path) -> PathBuf {
    codex_home.join("auth.json")
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn compute_codex_keychain_account(codex_home: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(codex_home.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    format!("cli|{}", &hex[..16])
}

fn extract_account_id_from_access_token(token: &str) -> Option<String> {
    let payload = decode_jwt_payload(token).ok()?;
    extract_account_id_from_payload(&payload)
}

fn extract_account_id_from_id_token(id_token: &Value) -> Option<String> {
    match id_token {
        Value::String(jwt) => decode_jwt_payload(jwt)
            .ok()
            .and_then(|payload| extract_account_id_from_payload(&payload)),
        Value::Object(payload) => extract_account_id_from_payload(&Value::Object(payload.clone())),
        _ => None,
    }
}

fn extract_account_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("chatgpt_account_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            payload
                .get("https://api.openai.com/auth")
                .and_then(|value| value.get("chatgpt_account_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn parse_jwt_expiration(token: &str) -> Result<Option<DateTime<Utc>>> {
    let payload = decode_jwt_payload(token)?;
    Ok(payload
        .get("exp")
        .and_then(Value::as_i64)
        .and_then(|exp| DateTime::<Utc>::from_timestamp(exp, 0)))
}

fn classify_refresh_failure_kind(body: &str) -> CodexOAuthRefreshFailureKind {
    match extract_refresh_error_code(body).as_deref() {
        Some("refresh_token_expired") => CodexOAuthRefreshFailureKind::Expired,
        Some("refresh_token_reused") => CodexOAuthRefreshFailureKind::Reused,
        Some("refresh_token_invalidated") | Some("refresh_token_revoked") => {
            CodexOAuthRefreshFailureKind::Revoked
        }
        _ => CodexOAuthRefreshFailureKind::Other,
    }
}

fn extract_refresh_error_code(body: &str) -> Option<String> {
    let Value::Object(map) = serde_json::from_str::<Value>(body).ok()? else {
        return None;
    };
    if let Some(error) = map.get("error") {
        match error {
            Value::String(code) => return Some(code.to_ascii_lowercase()),
            Value::Object(object) => {
                if let Some(code) = object.get("code").and_then(Value::as_str) {
                    return Some(code.to_ascii_lowercase());
                }
            }
            _ => {}
        }
    }
    map.get("code")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
}

fn try_parse_refresh_error_message(body: &str) -> Option<String> {
    let Value::Object(map) = serde_json::from_str::<Value>(body).ok()? else {
        return None;
    };
    map.get("error_description")
        .and_then(Value::as_str)
        .or_else(|| map.get("message").and_then(Value::as_str))
        .or_else(|| {
            map.get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
}

fn classify_device_poll_status(status: reqwest::StatusCode, body: &str) -> DevicePollStatus {
    let error_code = extract_refresh_error_code(body);
    // Terminal error codes take priority regardless of HTTP status.
    match error_code.as_deref() {
        Some("expired_token") => return DevicePollStatus::Expired,
        Some("access_denied") => return DevicePollStatus::AccessDenied,
        _ => {}
    }
    // Standard OAuth device-flow pending/slow_down codes.
    match error_code.as_deref() {
        Some("authorization_pending") => return DevicePollStatus::Pending,
        Some("slow_down") => return DevicePollStatus::SlowDown,
        _ => {}
    }
    // OpenAI returns HTTP 403 with a non-standard error code when device
    // authorization is still pending. Treat 403/404 as pending rather than
    // letting the unrecognized code fall through to Failed.
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return DevicePollStatus::Pending;
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return DevicePollStatus::SlowDown;
    }
    // Unknown failure: surface the parsed message or raw body.
    match error_code.as_deref() {
        Some(code) => DevicePollStatus::Failed(
            try_parse_refresh_error_message(body).unwrap_or_else(|| code.to_string()),
        ),
        None => DevicePollStatus::Failed(
            try_parse_refresh_error_message(body).unwrap_or_else(|| body.to_string()),
        ),
    }
}

fn decode_jwt_payload(token: &str) -> Result<Value> {
    let mut parts = token.split('.');
    let (_header, payload, _sig) = match (parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(sig))
            if !header.is_empty() && !payload.is_empty() && !sig.is_empty() =>
        {
            (header, payload, sig)
        }
        _ => return Err(anyhow!("invalid JWT format")),
    };
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .context("failed to decode JWT payload")?;
    serde_json::from_slice(&payload).context("failed to parse JWT payload")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use serde_json::json;

    fn encode_segment(value: serde_json::Value) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    }

    fn make_token(payload: serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            encode_segment(json!({"alg": "none"})),
            encode_segment(payload),
            encode_segment(json!("sig"))
        )
    }

    #[test]
    fn extracts_account_id_from_nested_access_claims() {
        let token = make_token(json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_nested"
            }
        }));

        assert_eq!(
            extract_account_id_from_access_token(&token).as_deref(),
            Some("acct_nested")
        );
    }

    #[test]
    fn extracts_account_id_from_chatgpt_account_id_claim() {
        let token = make_token(json!({
            "chatgpt_account_id": "acct_chatgpt"
        }));

        assert_eq!(
            extract_account_id_from_access_token(&token).as_deref(),
            Some("acct_chatgpt")
        );
    }

    #[test]
    fn auth_to_credential_uses_id_token_object_fallback() {
        let credential = auth_to_credential(
            AuthDotJson {
                tokens: Some(TokenData {
                    access_token: make_token(json!({
                        "exp": 1_900_000_000
                    })),
                    refresh_token: "refresh".to_string(),
                    account_id: None,
                    id_token: Some(json!({
                        "chatgpt_account_id": "acct_id_token"
                    })),
                }),
                last_refresh: None,
            },
            "file",
        )
        .expect("id_token object fallback should resolve account id");

        assert_eq!(credential.account_id, "acct_id_token");
        assert_eq!(credential.source, "file");
    }

    #[test]
    fn codex_oauth_login_result_material_is_profile_compatible() {
        let result = codex_oauth_tokens_to_login_result(LoginTokenResponse {
            id_token: make_token(json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": "acct_login"
                }
            })),
            access_token: make_token(json!({
                "exp": 1_900_000_000
            })),
            refresh_token: "refresh_login".to_string(),
        })
        .expect("login material should serialize");

        let credential = load_codex_oauth_profile_credential(&result.material, "openai-codex")
            .expect("login material should parse as a Holon OAuth profile");

        assert_eq!(result.account_id.as_deref(), Some("acct_login"));
        assert_eq!(credential.account_id, "acct_login");
        assert_eq!(credential.source, "credential_profile:openai-codex");
    }

    #[test]
    fn device_user_code_response_accepts_string_and_numeric_timing_fields() {
        let response: DeviceUserCodeResponse = serde_json::from_value(json!({
            "device_auth_id": "auth_123",
            "usercode": "ABCD-EFGH",
            "interval": "2",
            "expires_in": "600"
        }))
        .expect("string timing fields should parse");

        assert_eq!(response.device_auth_id, "auth_123");
        assert_eq!(response.user_code, "ABCD-EFGH");
        assert_eq!(response.interval, 2);
        assert_eq!(response.expires_in, Some(600));

        let response: DeviceUserCodeResponse = serde_json::from_value(json!({
            "device_auth_id": "auth_456",
            "user_code": "WXYZ-1234",
            "interval": 5,
            "expires_in": 300
        }))
        .expect("numeric timing fields should parse");

        assert_eq!(response.interval, 5);
        assert_eq!(response.expires_in, Some(300));
    }

    #[test]
    fn classify_device_poll_status_uses_oauth_error_codes() {
        assert_eq!(
            classify_device_poll_status(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"authorization_pending"}"#
            ),
            DevicePollStatus::Pending
        );
        assert_eq!(
            classify_device_poll_status(
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":"slow_down"}"#
            ),
            DevicePollStatus::SlowDown
        );
        assert_eq!(
            classify_device_poll_status(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"expired_token"}"#
            ),
            DevicePollStatus::Expired
        );
        assert_eq!(
            classify_device_poll_status(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"access_denied"}"#
            ),
            DevicePollStatus::AccessDenied
        );
        assert_eq!(
            classify_device_poll_status(reqwest::StatusCode::FORBIDDEN, ""),
            DevicePollStatus::Pending
        );
        assert_eq!(
            classify_device_poll_status(reqwest::StatusCode::TOO_MANY_REQUESTS, ""),
            DevicePollStatus::SlowDown
        );
        // OpenAI returns HTTP 403 with a non-standard error code when the
        // device authorization is still pending. This should be treated as
        // Pending, not Failed.
        assert_eq!(
            classify_device_poll_status(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"code":"forbidden","message":"Device authorization is pending. Please try again."}"#
            ),
            DevicePollStatus::Pending
        );
        // But access_denied on 403 should still be terminal.
        assert_eq!(
            classify_device_poll_status(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"error":"access_denied"}"#
            ),
            DevicePollStatus::AccessDenied
        );
    }

    #[test]
    fn keychain_account_matches_cli_prefix_shape() {
        let account = compute_codex_keychain_account(std::path::Path::new("/tmp/codex-home"));
        assert!(account.starts_with("cli|"));
        assert_eq!(account.len(), 20);
    }
}
