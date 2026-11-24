//! Persistent authentication records.

use anyhow::Result;
use chrono::{DateTime, Utc};
use holon_storage::authentication as storage;

use crate::authentication::{
    AuthSessionRecord, AuthUserRecord, BootstrapCredentialRecord, LoginTransactionRecord,
};
use crate::runtime_db::RuntimeDb;

pub struct AuthenticationRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

impl AuthenticationRepository<'_> {
    pub fn upsert_user(&self, user: &AuthUserRecord) -> Result<()> {
        self.db.transaction(|tx| storage::upsert_user(tx, user))
    }

    pub fn find_user(&self, issuer: &str, subject: &str) -> Result<Option<AuthUserRecord>> {
        self.db
            .with_read(|connection| storage::find_user(connection, issuer, subject))
    }

    pub fn find_user_by_id(&self, user_id: &str) -> Result<Option<AuthUserRecord>> {
        self.db
            .with_read(|connection| storage::find_user_by_id(connection, user_id))
    }

    pub fn create_session(&self, session: &AuthSessionRecord) -> Result<()> {
        self.db
            .transaction(|tx| storage::create_session(tx, session))
    }

    pub fn find_session(&self, session_digest: &str) -> Result<Option<AuthSessionRecord>> {
        self.db
            .with_read(|connection| storage::find_session(connection, session_digest))
    }

    pub fn touch_session(&self, session_digest: &str, last_seen_at: DateTime<Utc>) -> Result<bool> {
        self.db
            .transaction(|tx| storage::touch_session(tx, session_digest, last_seen_at))
    }

    pub(crate) fn checkpoint_session_activity(
        &self,
        sessions: &[(String, DateTime<Utc>)],
    ) -> Result<()> {
        self.db.transaction_with_context(
            crate::runtime_db::write_queue::RuntimeDbWriteContext::sync(
                "authentication.checkpoint_session_activity",
                "auth_sessions",
            ),
            |tx| storage::checkpoint_session_activity(tx, sessions),
        )
    }

    pub fn revoke_session(&self, session_digest: &str, revoked_at: DateTime<Utc>) -> Result<bool> {
        self.db
            .transaction(|tx| storage::revoke_session(tx, session_digest, revoked_at))
    }

    pub fn insert_bootstrap_credential(
        &self,
        credential: &BootstrapCredentialRecord,
    ) -> Result<()> {
        self.db
            .transaction(|tx| storage::insert_bootstrap_credential(tx, credential))
    }

    pub fn consume_bootstrap_credential(
        &self,
        credential_digest: &str,
        consumed_at: DateTime<Utc>,
    ) -> Result<Option<BootstrapCredentialRecord>> {
        self.consume_bootstrap_credential_for_scopes(credential_digest, consumed_at, None)
    }

    /// Proof validation and single-use consumption share one transaction.
    pub fn consume_bootstrap_credential_for_scopes(
        &self,
        credential_digest: &str,
        consumed_at: DateTime<Utc>,
        allowed_scopes: Option<&[&str]>,
    ) -> Result<Option<BootstrapCredentialRecord>> {
        self.db.transaction(|tx| {
            storage::consume_bootstrap_credential_for_scopes(
                tx,
                credential_digest,
                consumed_at,
                allowed_scopes,
            )
        })
    }

    pub fn insert_login_transaction(&self, login: &LoginTransactionRecord) -> Result<()> {
        self.db
            .transaction(|tx| storage::insert_login_transaction(tx, login))
    }

    pub fn consume_login_transaction(
        &self,
        state_digest: &str,
        consumed_at: DateTime<Utc>,
    ) -> Result<Option<LoginTransactionRecord>> {
        self.db
            .transaction(|tx| storage::consume_login_transaction(tx, state_digest, consumed_at))
    }
}

#[cfg(holon_lib_test_shard = "b")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_bootstrap_concurrent_redemption_has_one_winner() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            temp.path().join("runtime.sqlite"),
            temp.path().join("runtime.lock"),
        )?;
        let now = Utc::now();
        db.authentication()
            .insert_bootstrap_credential(&BootstrapCredentialRecord {
                credential_digest: "concurrent-native-code".into(),
                user_id: None,
                scope: "native-session:challenge".into(),
                created_at: now,
                expires_at: now + chrono::Duration::minutes(2),
                consumed_at: None,
                revoked_at: None,
            })?;
        let barrier = std::sync::Barrier::new(2);
        let winners = std::thread::scope(|scope| {
            let redeem = || {
                barrier.wait();
                db.authentication()
                    .consume_bootstrap_credential_for_scopes(
                        "concurrent-native-code",
                        now,
                        Some(&["native-session:challenge"]),
                    )
                    .map(|record| usize::from(record.is_some()))
            };
            let first = scope.spawn(redeem);
            let second = scope.spawn(redeem);
            Ok::<_, anyhow::Error>(first.join().unwrap()? + second.join().unwrap()?)
        })?;
        assert_eq!(winners, 1);
        Ok(())
    }
}
