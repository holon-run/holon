//! Persistent authentication records.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use crate::authentication::{
    AuthSessionRecord, AuthUserRecord, BootstrapCredentialRecord, LoginTransactionRecord,
};
use crate::runtime_db::RuntimeDb;

pub struct AuthenticationRepository<'a> {
    pub(crate) db: &'a RuntimeDb,
}

impl AuthenticationRepository<'_> {
    pub fn upsert_user(&self, user: &AuthUserRecord) -> Result<()> {
        self.db.transaction(|tx| {
            let existing_identity = tx
                .query_row(
                    "SELECT issuer, subject FROM auth_users WHERE user_id = ?1",
                    [user.user_id.as_str()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            if let Some((issuer, subject)) = existing_identity {
                if issuer != user.issuer || subject != user.subject {
                    anyhow::bail!(
                        "authentication user_id {} is already bound to a different issuer/subject",
                        user.user_id
                    );
                }
            }
            tx.execute(
                "INSERT INTO auth_users (
                    user_id, issuer, subject, display_name, email,
                    created_at, updated_at, disabled_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(user_id) DO UPDATE SET
                    issuer = excluded.issuer,
                    subject = excluded.subject,
                    display_name = excluded.display_name,
                    email = excluded.email,
                    updated_at = excluded.updated_at,
                    disabled_at = excluded.disabled_at",
                params![
                    user.user_id,
                    user.issuer,
                    user.subject,
                    user.display_name,
                    user.email,
                    timestamp(user.created_at),
                    timestamp(user.updated_at),
                    user.disabled_at.map(timestamp),
                ],
            )?;
            Ok(())
        })
    }

    pub fn find_user(&self, issuer: &str, subject: &str) -> Result<Option<AuthUserRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT user_id, issuer, subject, display_name, email,
                        created_at, updated_at, disabled_at
                 FROM auth_users WHERE issuer = ?1 AND subject = ?2",
                params![issuer, subject],
                row_to_user,
            )
            .optional()
            .context("looking up authenticated user")
    }

    pub fn create_session(&self, session: &AuthSessionRecord) -> Result<()> {
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO auth_sessions (
                    session_digest, user_id, auth_method,
                    created_at, expires_at, last_seen_at, revoked_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    session.session_digest,
                    session.user_id,
                    session.auth_method,
                    timestamp(session.created_at),
                    timestamp(session.expires_at),
                    timestamp(session.last_seen_at),
                    session.revoked_at.map(timestamp),
                ],
            )?;
            Ok(())
        })
    }

    pub fn find_session(&self, session_digest: &str) -> Result<Option<AuthSessionRecord>> {
        let connection = self.db.connection()?;
        connection
            .query_row(
                "SELECT session_digest, user_id, auth_method,
                        created_at, expires_at, last_seen_at, revoked_at
                 FROM auth_sessions WHERE session_digest = ?1",
                [session_digest],
                row_to_session,
            )
            .optional()
            .context("looking up authentication session")
    }

    pub fn touch_session(&self, session_digest: &str, last_seen_at: DateTime<Utc>) -> Result<bool> {
        self.db.transaction(|tx| {
            Ok(tx.execute(
                "UPDATE auth_sessions
                 SET last_seen_at = ?2
                 WHERE session_digest = ?1
                   AND revoked_at IS NULL
                   AND expires_at > ?2
                   AND last_seen_at <= ?2",
                params![session_digest, timestamp(last_seen_at)],
            )? == 1)
        })
    }

    pub fn revoke_session(&self, session_digest: &str, revoked_at: DateTime<Utc>) -> Result<bool> {
        self.db.transaction(|tx| {
            Ok(tx.execute(
                "UPDATE auth_sessions
                 SET revoked_at = ?2
                 WHERE session_digest = ?1 AND revoked_at IS NULL",
                params![session_digest, timestamp(revoked_at)],
            )? == 1)
        })
    }

    pub fn insert_bootstrap_credential(
        &self,
        credential: &BootstrapCredentialRecord,
    ) -> Result<()> {
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO auth_bootstrap_credentials (
                    credential_digest, user_id, scope,
                    created_at, expires_at, consumed_at, revoked_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    credential.credential_digest,
                    credential.user_id,
                    credential.scope,
                    timestamp(credential.created_at),
                    timestamp(credential.expires_at),
                    credential.consumed_at.map(timestamp),
                    credential.revoked_at.map(timestamp),
                ],
            )?;
            Ok(())
        })
    }

    pub fn consume_bootstrap_credential(
        &self,
        credential_digest: &str,
        consumed_at: DateTime<Utc>,
    ) -> Result<Option<BootstrapCredentialRecord>> {
        self.db.transaction(|tx| {
            let updated = tx.execute(
                "UPDATE auth_bootstrap_credentials
                 SET consumed_at = ?2
                 WHERE credential_digest = ?1
                   AND consumed_at IS NULL
                   AND revoked_at IS NULL
                   AND expires_at > ?2",
                params![credential_digest, timestamp(consumed_at)],
            )?;
            if updated == 0 {
                return Ok(None);
            }
            tx.query_row(
                "SELECT credential_digest, user_id, scope,
                        created_at, expires_at, consumed_at, revoked_at
                 FROM auth_bootstrap_credentials
                 WHERE credential_digest = ?1",
                [credential_digest],
                row_to_bootstrap,
            )
            .map(Some)
            .context("reading consumed bootstrap credential")
        })
    }

    pub fn insert_login_transaction(&self, login: &LoginTransactionRecord) -> Result<()> {
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO auth_login_transactions (
                    transaction_digest, state_digest, nonce_digest,
                    code_verifier, created_at, expires_at, consumed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    login.transaction_digest,
                    login.state_digest,
                    login.nonce_digest,
                    login.code_verifier,
                    timestamp(login.created_at),
                    timestamp(login.expires_at),
                    login.consumed_at.map(timestamp),
                ],
            )?;
            Ok(())
        })
    }

    pub fn consume_login_transaction(
        &self,
        state_digest: &str,
        consumed_at: DateTime<Utc>,
    ) -> Result<Option<LoginTransactionRecord>> {
        self.db.transaction(|tx| {
            let updated = tx.execute(
                "UPDATE auth_login_transactions
                 SET consumed_at = ?2
                 WHERE state_digest = ?1
                   AND consumed_at IS NULL
                   AND expires_at > ?2",
                params![state_digest, timestamp(consumed_at)],
            )?;
            if updated == 0 {
                return Ok(None);
            }
            tx.query_row(
                "SELECT transaction_digest, state_digest, nonce_digest,
                        code_verifier, created_at, expires_at, consumed_at
                 FROM auth_login_transactions
                 WHERE state_digest = ?1",
                [state_digest],
                |row| {
                    Ok(LoginTransactionRecord {
                        transaction_digest: row.get(0)?,
                        state_digest: row.get(1)?,
                        nonce_digest: row.get(2)?,
                        code_verifier: row.get(3)?,
                        created_at: parse_timestamp(row.get(4)?)?,
                        expires_at: parse_timestamp(row.get(5)?)?,
                        consumed_at: row
                            .get::<_, Option<String>>(6)?
                            .map(parse_timestamp)
                            .transpose()?,
                    })
                },
            )
            .map(Some)
            .context("reading consumed login transaction")
        })
    }
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_timestamp(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                value.len(),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn row_to_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuthUserRecord> {
    Ok(AuthUserRecord {
        user_id: row.get(0)?,
        issuer: row.get(1)?,
        subject: row.get(2)?,
        display_name: row.get(3)?,
        email: row.get(4)?,
        created_at: parse_timestamp(row.get(5)?)?,
        updated_at: parse_timestamp(row.get(6)?)?,
        disabled_at: row
            .get::<_, Option<String>>(7)?
            .map(parse_timestamp)
            .transpose()?,
    })
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuthSessionRecord> {
    Ok(AuthSessionRecord {
        session_digest: row.get(0)?,
        user_id: row.get(1)?,
        auth_method: row.get(2)?,
        created_at: parse_timestamp(row.get(3)?)?,
        expires_at: parse_timestamp(row.get(4)?)?,
        last_seen_at: parse_timestamp(row.get(5)?)?,
        revoked_at: row
            .get::<_, Option<String>>(6)?
            .map(parse_timestamp)
            .transpose()?,
    })
}

fn row_to_bootstrap(row: &rusqlite::Row<'_>) -> rusqlite::Result<BootstrapCredentialRecord> {
    Ok(BootstrapCredentialRecord {
        credential_digest: row.get(0)?,
        user_id: row.get(1)?,
        scope: row.get(2)?,
        created_at: parse_timestamp(row.get(3)?)?,
        expires_at: parse_timestamp(row.get(4)?)?,
        consumed_at: row
            .get::<_, Option<String>>(5)?
            .map(parse_timestamp)
            .transpose()?,
        revoked_at: row
            .get::<_, Option<String>>(6)?
            .map(parse_timestamp)
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_db::RuntimeDb;

    #[test]
    fn bootstrap_credential_is_consumed_once() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            temp_dir.path().join("runtime.sqlite"),
            temp_dir.path().join("runtime.lock"),
        )?;
        let now = Utc::now();
        db.authentication()
            .insert_bootstrap_credential(&BootstrapCredentialRecord {
                credential_digest: "digest".into(),
                user_id: None,
                scope: "session_exchange".into(),
                created_at: now,
                expires_at: now + chrono::Duration::hours(1),
                consumed_at: None,
                revoked_at: None,
            })?;

        assert!(db
            .authentication()
            .consume_bootstrap_credential("digest", now)?
            .is_some());
        assert!(db
            .authentication()
            .consume_bootstrap_credential("digest", now)?
            .is_none());
        Ok(())
    }

    #[test]
    fn touching_session_does_not_move_last_seen_backwards() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            temp_dir.path().join("runtime.sqlite"),
            temp_dir.path().join("runtime.lock"),
        )?;
        let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis())
            .expect("valid timestamp");
        db.authentication().upsert_user(&AuthUserRecord {
            user_id: "user".into(),
            issuer: "local".into(),
            subject: "user".into(),
            display_name: None,
            email: None,
            created_at: now,
            updated_at: now,
            disabled_at: None,
        })?;
        db.authentication().create_session(&AuthSessionRecord {
            session_digest: "session".into(),
            user_id: "user".into(),
            auth_method: "local".into(),
            created_at: now,
            expires_at: now + chrono::Duration::hours(1),
            last_seen_at: now + chrono::Duration::minutes(5),
            revoked_at: None,
        })?;

        assert!(!db.authentication().touch_session("session", now)?);
        assert_eq!(
            db.authentication()
                .find_session("session")?
                .expect("session exists")
                .last_seen_at,
            chrono::DateTime::from_timestamp_millis(
                (now + chrono::Duration::minutes(5)).timestamp_millis()
            )
            .expect("valid timestamp")
        );
        Ok(())
    }

    #[test]
    fn user_identity_cannot_be_rebound_to_another_external_identity() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            temp_dir.path().join("runtime.sqlite"),
            temp_dir.path().join("runtime.lock"),
        )?;
        let now = Utc::now();
        let user = AuthUserRecord {
            user_id: "user".into(),
            issuer: "https://issuer.example".into(),
            subject: "subject-a".into(),
            display_name: None,
            email: None,
            created_at: now,
            updated_at: now,
            disabled_at: None,
        };
        db.authentication().upsert_user(&user)?;

        let mut rebound = user;
        rebound.subject = "subject-b".into();
        let error = db.authentication().upsert_user(&rebound).unwrap_err();
        assert!(error.to_string().contains("already bound"));
        Ok(())
    }
}
