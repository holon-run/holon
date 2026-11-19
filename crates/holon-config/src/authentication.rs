use std::time::Duration;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
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
                && !(redirect.scheme() == "http" && is_loopback_host(&redirect))
            {
                bail!(
                    "OIDC redirect_uri must use HTTPS or target a loopback host (localhost, 127.0.0.1, or ::1)"
                );
            }
        }
        Ok(())
    }
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPolicy {
    pub absolute_ttl_seconds: Option<u64>,
    pub idle_ttl_seconds: u64,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            absolute_ttl_seconds: None,
            idle_ttl_seconds: Duration::from_secs(7 * 24 * 60 * 60).as_secs(),
        }
    }
}

impl SessionPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.idle_ttl_seconds == 0 {
            bail!("session idle TTL must be greater than zero");
        }
        if let Some(absolute_ttl_seconds) = self.absolute_ttl_seconds {
            if absolute_ttl_seconds == 0 {
                bail!("session absolute TTL must be greater than zero or null for unlimited");
            }
            if self.idle_ttl_seconds > absolute_ttl_seconds {
                bail!("session idle TTL must not exceed absolute TTL");
            }
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

#[doc(hidden)]
pub fn materialize_auth_config(file: &crate::AuthConfigFile) -> Result<AuthConfig> {
    let defaults = SessionPolicy::default();
    let absolute_ttl_seconds = file
        .session
        .absolute_ttl_seconds
        .and_then(|value| (value != 0).then_some(value))
        .or(defaults.absolute_ttl_seconds);
    // Clamp the derived idle default to an explicit absolute TTL so configs
    // persisted before a default bump stay loadable.
    let idle_ttl_seconds = file.session.idle_ttl_seconds.unwrap_or_else(|| {
        absolute_ttl_seconds.map_or(defaults.idle_ttl_seconds, |absolute| {
            defaults.idle_ttl_seconds.min(absolute)
        })
    });
    let session = SessionPolicy {
        absolute_ttl_seconds,
        idle_ttl_seconds,
    };
    let oidc = file.oidc.as_ref().map(|oidc| OidcProviderConfig {
        issuer_url: oidc.issuer_url.clone(),
        client_id: oidc.client_id.clone(),
        client_secret_env: oidc.client_secret_env.clone(),
        redirect_uri: oidc.redirect_uri.clone(),
    });
    let config = AuthConfig {
        mode: file.mode.unwrap_or(AuthenticationMode::Local),
        oidc,
        session,
    };
    config.validate()?;
    Ok(config)
}

#[cfg(test)]
mod tests;
