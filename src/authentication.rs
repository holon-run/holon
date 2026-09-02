//! Authentication domain types shared by configuration and runtime storage.
//!
//! Protocol handling belongs to the HTTP authentication layer. This module
//! deliberately contains no transport or provider-specific code.

use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationMode {
    #[default]
    Local,
    Oidc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcProviderConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret_env: Option<String>,
    pub redirect_uri: Option<String>,
}

impl OidcProviderConfig {
    pub fn validate(&self) -> Result<()> {
        let issuer = Url::parse(&self.issuer_url)?;
        if issuer.scheme() != "https" {
            bail!("OIDC issuer_url must use HTTPS");
        }
        if self.client_id.trim().is_empty() {
            bail!("OIDC client_id must not be empty");
        }
        if let Some(redirect_uri) = &self.redirect_uri {
            let redirect = Url::parse(redirect_uri)?;
            if redirect.scheme() != "https"
                && !(redirect.scheme() == "http" && redirect.host_str() == Some("localhost"))
            {
                bail!("OIDC redirect_uri must use HTTPS or target localhost");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPolicy {
    pub absolute_ttl_seconds: u64,
    pub idle_ttl_seconds: u64,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            absolute_ttl_seconds: Duration::from_secs(8 * 60 * 60).as_secs(),
            idle_ttl_seconds: Duration::from_secs(30 * 60).as_secs(),
        }
    }
}

impl SessionPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.absolute_ttl_seconds == 0 || self.idle_ttl_seconds == 0 {
            bail!("session TTLs must be greater than zero");
        }
        if self.idle_ttl_seconds > self.absolute_ttl_seconds {
            bail!("session idle TTL must not exceed absolute TTL");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub mode: AuthenticationMode,
    pub oidc: Option<OidcProviderConfig>,
    pub session: SessionPolicy,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthenticationMode::Local,
            oidc: None,
            session: SessionPolicy::default(),
        }
    }
}

impl AuthConfig {
    pub fn validate(&self) -> Result<()> {
        self.session.validate()?;
        match (self.mode, &self.oidc) {
            (AuthenticationMode::Local, None) => Ok(()),
            (AuthenticationMode::Local, Some(_)) => {
                bail!("OIDC configuration is not allowed when auth mode is local")
            }
            (AuthenticationMode::Oidc, Some(oidc)) => oidc.validate(),
            (AuthenticationMode::Oidc, None) => {
                bail!("OIDC configuration is required when auth mode is oidc")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUserRecord {
    pub user_id: String,
    pub issuer: String,
    pub subject: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSessionRecord {
    pub session_digest: String,
    pub user_id: String,
    pub auth_method: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl AuthSessionRecord {
    pub fn is_active_at(&self, now: DateTime<Utc>, idle_ttl: Duration) -> bool {
        self.revoked_at.is_none()
            && self.expires_at > now
            && self.last_seen_at + chrono::Duration::from_std(idle_ttl).unwrap_or_default() > now
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapCredentialRecord {
    pub credential_digest: String,
    pub user_id: Option<String>,
    pub scope: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl BootstrapCredentialRecord {
    pub fn is_redeemable_at(&self, now: DateTime<Utc>) -> bool {
        self.consumed_at.is_none() && self.revoked_at.is_none() && self.expires_at > now
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginTransactionRecord {
    pub transaction_digest: String,
    pub state_digest: String,
    pub nonce_digest: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
}

pub fn digest_secret(secret: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(secret.as_bytes());
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_without_exposing_secret() {
        let digest = digest_secret("bootstrap-secret");
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, "bootstrap-secret");
        assert_eq!(digest, digest_secret("bootstrap-secret"));
    }

    #[test]
    fn oidc_configuration_requires_https() {
        let config = OidcProviderConfig {
            issuer_url: "http://issuer.example".into(),
            client_id: "holon".into(),
            client_secret_env: None,
            redirect_uri: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn localhost_redirect_must_use_http() {
        let mut config = OidcProviderConfig {
            issuer_url: "https://issuer.example".into(),
            client_id: "client".into(),
            client_secret_env: None,
            redirect_uri: Some("ftp://localhost/callback".into()),
        };
        assert!(config.validate().is_err());

        config.redirect_uri = Some("http://localhost/callback".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn local_auth_is_default_and_valid() {
        assert_eq!(AuthConfig::default().mode, AuthenticationMode::Local);
        assert!(AuthConfig::default().validate().is_ok());
    }
}
