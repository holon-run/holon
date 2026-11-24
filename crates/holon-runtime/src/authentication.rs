//! Authentication domain types shared by configuration and runtime storage.
//!
//! Protocol handling belongs to the HTTP authentication layer. This module
//! deliberately contains no transport or provider-specific code.

pub use holon_config::authentication::{
    AuthConfig, AuthenticationMode, OidcProviderConfig, SessionPolicy,
};
use sha2::{Digest, Sha256};

pub(crate) mod activity;

pub use holon_storage::authentication::{
    AuthSessionRecord, AuthUserRecord, BootstrapCredentialRecord, LoginTransactionRecord,
};

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
