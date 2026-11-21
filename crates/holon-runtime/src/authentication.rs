//! Authentication domain types shared by configuration and runtime storage.
//!
//! Protocol handling belongs to the HTTP authentication layer. This module
//! deliberately contains no transport or provider-specific code.

use std::time::Duration;

use chrono::{DateTime, Utc};
pub use holon_config::authentication::{
    AuthConfig, AuthenticationMode, OidcProviderConfig, SessionPolicy,
};
use sha2::{Digest, Sha256};

pub(crate) mod activity;

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
    pub expires_at: Option<DateTime<Utc>>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl AuthSessionRecord {
    pub fn is_active_at(&self, now: DateTime<Utc>, idle_ttl: Duration) -> bool {
        self.revoked_at.is_none()
            && self.expires_at.is_none_or(|expires_at| expires_at > now)
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
    pub code_verifier: String,
    pub native_redirect_uri: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
}

pub fn digest_secret(secret: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(secret.as_bytes());
    format!("{:x}", digest.finalize())
}

#[cfg(holon_lib_test_shard = "d")]
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
}
