//! OIDC authorization-code flow and session issuance.
//!
//! This module intentionally stops at issuing Holon sessions. HTTP handlers,
//! cookies, and request extractors belong to the HTTP integration layer.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::authentication::{
    digest_secret, AuthConfig, AuthSessionRecord, AuthUserRecord, LoginTransactionRecord,
};
use crate::runtime_db::RuntimeDb;

const MAX_DISCOVERY_BYTES: u64 = 1024 * 1024;
const MAX_JWKS_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TOKEN_RESPONSE_BYTES: u64 = 1024 * 1024;
const ID_TOKEN_LEEWAY_SECONDS: i64 = 5;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct OidcDiscovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TokenResponse {
    id_token: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    n: Option<String>,
    e: Option<String>,
    alg: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IdTokenClaims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: i64,
    iat: i64,
    nbf: Option<i64>,
    nonce: Option<String>,
    azp: Option<String>,
    name: Option<String>,
    preferred_username: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, value: &str) -> bool {
        match self {
            Self::One(aud) => aud == value,
            Self::Many(aud) => aud.iter().any(|aud| aud == value),
        }
    }

    fn requires_authorized_party(&self) -> bool {
        matches!(self, Self::Many(aud) if aud.len() > 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginStart {
    pub state: String,
    pub nonce: String,
    pub code_verifier: String,
    pub authorization_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedSession {
    pub credential: String,
    pub record: AuthSessionRecord,
}

#[derive(Debug, Clone)]
pub struct OidcClient {
    pub config: AuthConfig,
    pub http: reqwest::Client,
}

impl OidcClient {
    pub fn new(config: AuthConfig) -> Result<Self> {
        config.validate()?;
        if config
            .oidc
            .as_ref()
            .is_some_and(|oidc| oidc.redirect_uri.is_none())
        {
            bail!("OIDC redirect_uri is required");
        }
        Ok(Self {
            config,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()?,
        })
    }

    pub async fn discover(&self) -> Result<OidcDiscovery> {
        let oidc = self
            .config
            .oidc
            .as_ref()
            .context("OIDC is not configured")?;
        let endpoint = Url::parse(&format!(
            "{}/.well-known/openid-configuration",
            oidc.issuer_url.trim_end_matches('/')
        ))
        .context("building OIDC discovery URL")?;
        let response = self.http.get(endpoint).send().await?;
        if !response.status().is_success() {
            bail!("OIDC discovery failed with HTTP {}", response.status());
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_DISCOVERY_BYTES)
        {
            bail!("OIDC discovery response is too large");
        }
        let document =
            read_limited_body(response, MAX_DISCOVERY_BYTES, "OIDC discovery response").await?;
        let discovery: OidcDiscovery =
            serde_json::from_slice(&document).context("parsing OIDC discovery response")?;
        if discovery.issuer != oidc.issuer_url {
            bail!("OIDC discovery issuer does not match configured issuer");
        }
        for endpoint in [
            &discovery.authorization_endpoint,
            &discovery.token_endpoint,
            &discovery.jwks_uri,
        ] {
            let url = Url::parse(endpoint)?;
            if url.scheme() != "https" {
                bail!("OIDC endpoints must use HTTPS");
            }
        }
        Ok(discovery)
    }

    pub async fn begin_login(&self, db: &RuntimeDb, now: DateTime<Utc>) -> Result<LoginStart> {
        let discovery = self.discover().await?;
        let state = random_secret();
        let nonce = random_secret();
        let code_verifier = random_secret();
        let transaction_digest = digest_secret(&random_secret());
        let transaction = LoginTransactionRecord {
            transaction_digest,
            state_digest: digest_secret(&state),
            nonce_digest: digest_secret(&nonce),
            code_verifier: code_verifier.clone(),
            created_at: now,
            expires_at: now + chrono::Duration::minutes(10),
            consumed_at: None,
        };
        db.authentication().insert_login_transaction(&transaction)?;

        let oidc = self.config.oidc.as_ref().expect("validated OIDC config");
        let redirect_uri = oidc
            .redirect_uri
            .as_deref()
            .context("OIDC redirect_uri is required")?;
        let challenge = pkce_challenge(&code_verifier);
        let mut url = Url::parse(&discovery.authorization_endpoint)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &oidc.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", "openid profile email")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(LoginStart {
            state,
            nonce,
            code_verifier,
            authorization_url: url.to_string(),
        })
    }

    pub async fn complete_login(
        &self,
        db: &RuntimeDb,
        state: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<IssuedSession> {
        if code.is_empty() || state.is_empty() {
            bail!("OIDC callback is missing required parameters");
        }
        let transaction = db
            .authentication()
            .consume_login_transaction(&digest_secret(state), now)?
            .context("OIDC login transaction is invalid or expired")?;
        let discovery = self.discover().await?;
        let oidc = self.config.oidc.as_ref().expect("validated OIDC config");
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("client_id", oidc.client_id.clone()),
            (
                "redirect_uri",
                oidc.redirect_uri
                    .clone()
                    .context("OIDC redirect_uri is required")?,
            ),
            ("code_verifier", transaction.code_verifier.clone()),
        ];
        if let Some(env_name) = &oidc.client_secret_env {
            let secret = std::env::var(env_name).with_context(|| {
                format!("OIDC client secret environment variable {env_name} is missing")
            })?;
            form.push(("client_secret", secret));
        }
        let response = self
            .http
            .post(&discovery.token_endpoint)
            .form(&form)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("OIDC token exchange failed with HTTP {}", response.status());
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_TOKEN_RESPONSE_BYTES)
        {
            bail!("OIDC token response is too large");
        }
        let token_response =
            read_limited_body(response, MAX_TOKEN_RESPONSE_BYTES, "OIDC token response").await?;
        let tokens: TokenResponse =
            serde_json::from_slice(&token_response).context("parsing OIDC token response")?;
        let claims = self
            .validate_id_token(&discovery, &tokens.id_token, &transaction, now)
            .await?;
        let user_id = if let Some(user) = db.authentication().find_user(&claims.iss, &claims.sub)? {
            if user.disabled_at.is_some() {
                bail!("OIDC principal is disabled");
            }
            user.user_id
        } else {
            format!("oidc-{}", Uuid::new_v4())
        };
        let display_name = claims
            .name
            .or(claims.preferred_username)
            .or(claims.email.clone());
        db.authentication().upsert_user(&AuthUserRecord {
            user_id: user_id.clone(),
            issuer: claims.iss,
            subject: claims.sub,
            display_name,
            email: claims.email,
            created_at: now,
            updated_at: now,
            disabled_at: None,
        })?;
        issue_session(db, &self.config, &user_id, "oidc", now)
    }

    async fn validate_id_token(
        &self,
        discovery: &OidcDiscovery,
        token: &str,
        transaction: &LoginTransactionRecord,
        now: DateTime<Utc>,
    ) -> Result<IdTokenClaims> {
        let header = decode_header(token).context("invalid OIDC ID Token header")?;
        let key_set = self.http.get(&discovery.jwks_uri).send().await?;
        if !key_set.status().is_success() {
            bail!("OIDC JWKS request failed with HTTP {}", key_set.status());
        }
        if key_set
            .content_length()
            .is_some_and(|size| size > MAX_JWKS_BYTES)
        {
            bail!("OIDC JWKS response is too large");
        }
        let bytes = read_limited_body(key_set, MAX_JWKS_BYTES, "OIDC JWKS response").await?;
        let keys: Jwks = serde_json::from_slice(&bytes).context("parsing OIDC JWKS response")?;
        let jwk = keys
            .keys
            .iter()
            .find(|key| {
                key.kty == "RSA"
                    && key.kid.as_deref() == header.kid.as_deref()
                    && key.alg.as_deref().is_none_or(|alg| alg == "RS256")
            })
            .context("OIDC signing key was not found")?;
        let key = DecodingKey::from_rsa_components(
            jwk.n.as_deref().context("OIDC JWK is missing modulus")?,
            jwk.e.as_deref().context("OIDC JWK is missing exponent")?,
        )?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[discovery.issuer.as_str()]);
        validation.set_audience(&[self
            .config
            .oidc
            .as_ref()
            .expect("validated")
            .client_id
            .as_str()]);
        validation.leeway = ID_TOKEN_LEEWAY_SECONDS as u64;
        let claims = decode::<IdTokenClaims>(token, &key, &validation)
            .context("OIDC ID Token signature or claims validation failed")?
            .claims;
        let client_id = self
            .config
            .oidc
            .as_ref()
            .expect("validated")
            .client_id
            .as_str();
        if claims.exp <= now.timestamp() - ID_TOKEN_LEEWAY_SECONDS
            || claims
                .nbf
                .is_some_and(|value| value > now.timestamp() + ID_TOKEN_LEEWAY_SECONDS)
            || claims.iat > now.timestamp() + ID_TOKEN_LEEWAY_SECONDS
            || !claims.aud.contains(client_id)
            || (claims.aud.requires_authorized_party() && claims.azp.as_deref() != Some(client_id))
            || claims
                .nonce
                .as_deref()
                .is_none_or(|nonce| digest_secret(nonce) != transaction.nonce_digest)
        {
            bail!("OIDC ID Token claims are invalid");
        }
        Ok(claims)
    }
}

pub fn issue_session(
    db: &RuntimeDb,
    config: &AuthConfig,
    user_id: &str,
    auth_method: &str,
    now: DateTime<Utc>,
) -> Result<IssuedSession> {
    config.session.validate()?;
    let credential = random_secret();
    let ttl_seconds = i64::try_from(config.session.absolute_ttl_seconds)
        .context("session absolute TTL is too large")?;
    let ttl = chrono::Duration::try_seconds(ttl_seconds)
        .context("session absolute TTL is out of range")?;
    let expires_at = now
        .checked_add_signed(ttl)
        .context("session expiration is out of range")?;
    let record = AuthSessionRecord {
        session_digest: digest_secret(&credential),
        user_id: user_id.to_string(),
        auth_method: auth_method.to_string(),
        created_at: now,
        expires_at,
        last_seen_at: now,
        revoked_at: None,
    };
    db.authentication().create_session(&record)?;
    Ok(IssuedSession { credential, record })
}

pub fn exchange_bootstrap(
    db: &RuntimeDb,
    config: &AuthConfig,
    credential: &str,
    now: DateTime<Utc>,
) -> Result<IssuedSession> {
    let bootstrap = db
        .authentication()
        .consume_bootstrap_credential(&digest_secret(credential), now)?
        .context("bootstrap credential is invalid or expired")?;
    if bootstrap.scope != "session" && bootstrap.scope != "recovery" {
        bail!("bootstrap credential cannot create a session");
    }
    let user_id = bootstrap
        .user_id
        .context("bootstrap credential has no user")?;
    issue_session(db, config, &user_id, "bootstrap", now)
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn pkce_challenge(verifier: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

async fn read_limited_body(
    mut response: reqwest::Response,
    max_bytes: u64,
    description: &str,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > max_bytes)
    {
        bail!("{description} is too large");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .context("response body length overflow")?;
        if u64::try_from(next_len).is_ok_and(|size| size > max_bytes) {
            bail!("{description} is too large");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_uses_base64url_without_padding() {
        let challenge = pkce_challenge("test-verifier");
        assert!(!challenge.contains('='));
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
    }

    #[test]
    fn audience_supports_single_and_multiple_values() {
        assert!(Audience::One("client".into()).contains("client"));
        assert!(Audience::Many(vec!["other".into(), "client".into()]).contains("client"));
        assert!(!Audience::One("client".into()).requires_authorized_party());
        assert!(!Audience::Many(vec!["client".into()]).requires_authorized_party());
        assert!(Audience::Many(vec!["client".into(), "other".into()]).requires_authorized_party());
    }

    #[test]
    fn validates_a_real_rs256_id_token_signature() {
        let token = "eyJhbGciOiJSUzI1NiIsImtpZCI6InRlc3Qta2V5IiwidHlwIjoiSldUIn0.eyJpc3MiOiJodHRwczovL2lzc3Vlci5leGFtcGxlIiwic3ViIjoidXNlci0xIiwiYXVkIjoiY2xpZW50LWlkIiwiZXhwIjo0MTAyNDQ0ODAwLCJpYXQiOjE3MDAwMDAwMDB9.DInTkMDwCEWkq8ur4VROtKtjoAoVgdA3FoFxcANuUNTYeCL_vFok_ygkAy8uXXQvhg5FucM8bNil2XHoAgSQH4xfP2vDI-S0ZkcKWv-15vYjp_oQZ9wEJiwqGp-yBiJ9UdvZeS5MLVFDt8MWxlznm8UUlAvteOyDf8Hv7YmIHkYTQoYEmTSMi2jK5zFryLn_vjeW9r2sIWjj5OTzxF-Tvx1dgv7sLOzYl90dIL3Fg7_ZuWqgsDVPrfLFx_ygH9QBKB5LjLXmUkqgasQiF52fwdDhPXDS3z9oi40yPvsTovGF1Z0DgGY9NBteHgGVn6fqOfMpC3blDRfeIArzdGxjNg";
        let key = DecodingKey::from_rsa_components(
            "tB64pcpgi1oFG8JlacqqFcXtG2YYzAQxhRJbTwOZhf101qwsyjt89NE_u2RV8jKp9jjpsKizK-FMKEEZ5EPpGHEF4Y63djA1MJ3pNp3dY9tzK3ZvfEUSApDEgVvok_zXqiRok_CntIpcCJDaPF2GOULOHrBEeEZq4h0P7uQqxrgOaCv-R_jW1ycRynYmneLhCHlOEKYzOn6wOzQZ2qRNmcl5c_wBtwtpBLFLh4WwmjvYQKamJDqEowlirjYKjzrSuTWcDJG2Lbs7F0sVZzTdIdoao2q32C3y1GlVgPQCPcCOflt3YG73KNFLIa08B77nzajZJEAdUT6xQ09W0WzCbw",
            "AQAB",
        )
        .expect("valid RSA JWK components");
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&["https://issuer.example"]);
        validation.set_audience(&["client-id"]);

        let claims = decode::<IdTokenClaims>(token, &key, &validation)
            .expect("RSA ID Token signature should validate")
            .claims;

        assert_eq!(claims.iss, "https://issuer.example");
        assert_eq!(claims.sub, "user-1");
        assert!(claims.aud.contains("client-id"));
    }
}
