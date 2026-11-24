//! SQLite transactions for authentication records.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

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

pub fn upsert_user(tx: &Transaction<'_>, user: &AuthUserRecord) -> Result<()> {
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
}

pub fn find_user(
    connection: &Connection,
    issuer: &str,
    subject: &str,
) -> Result<Option<AuthUserRecord>> {
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

pub fn find_user_by_id(connection: &Connection, user_id: &str) -> Result<Option<AuthUserRecord>> {
    connection
        .query_row(
            "SELECT user_id, issuer, subject, display_name, email,
                    created_at, updated_at, disabled_at
             FROM auth_users WHERE user_id = ?1",
            [user_id],
            row_to_user,
        )
        .optional()
        .context("looking up authenticated user by id")
}

pub fn create_session(tx: &Transaction<'_>, session: &AuthSessionRecord) -> Result<()> {
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
            session.expires_at.map(timestamp),
            timestamp(session.last_seen_at),
            session.revoked_at.map(timestamp),
        ],
    )?;
    Ok(())
}

pub fn find_session(
    connection: &Connection,
    session_digest: &str,
) -> Result<Option<AuthSessionRecord>> {
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

pub fn touch_session(
    tx: &Transaction<'_>,
    session_digest: &str,
    last_seen_at: DateTime<Utc>,
) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE auth_sessions
         SET last_seen_at = ?2
         WHERE session_digest = ?1
           AND revoked_at IS NULL
            AND (expires_at IS NULL OR expires_at > ?2)
           AND last_seen_at <= ?2",
        params![session_digest, timestamp(last_seen_at)],
    )? == 1)
}

pub fn checkpoint_session_activity(
    tx: &Transaction<'_>,
    sessions: &[(String, DateTime<Utc>)],
) -> Result<()> {
    let mut statement = tx.prepare_cached(
        "UPDATE auth_sessions SET last_seen_at = ?2
         WHERE session_digest = ?1 AND revoked_at IS NULL
           AND (expires_at IS NULL OR expires_at > ?2)
           AND last_seen_at < ?2",
    )?;
    for (digest, last_seen_at) in sessions {
        statement.execute(params![digest, timestamp(*last_seen_at)])?;
    }
    Ok(())
}

pub fn revoke_session(
    tx: &Transaction<'_>,
    session_digest: &str,
    revoked_at: DateTime<Utc>,
) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE auth_sessions
         SET revoked_at = ?2
         WHERE session_digest = ?1 AND revoked_at IS NULL",
        params![session_digest, timestamp(revoked_at)],
    )? == 1)
}

pub fn insert_bootstrap_credential(
    tx: &Transaction<'_>,
    credential: &BootstrapCredentialRecord,
) -> Result<()> {
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
}

/// Proof validation and single-use consumption share one transaction.
pub fn consume_bootstrap_credential_for_scopes(
    tx: &Transaction<'_>,
    credential_digest: &str,
    consumed_at: DateTime<Utc>,
    allowed_scopes: Option<&[&str]>,
) -> Result<Option<BootstrapCredentialRecord>> {
    if let Some(scopes) = allowed_scopes {
        let scope: Option<String> = tx
            .query_row(
                "SELECT scope FROM auth_bootstrap_credentials WHERE credential_digest = ?1",
                [credential_digest],
                |row| row.get(0),
            )
            .optional()?;
        if !scope
            .as_deref()
            .is_some_and(|value| scopes.contains(&value))
        {
            return Ok(None);
        }
    }
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
}

pub fn insert_login_transaction(
    tx: &Transaction<'_>,
    login: &LoginTransactionRecord,
) -> Result<()> {
    tx.execute(
        "INSERT INTO auth_login_transactions (
            transaction_digest, state_digest, nonce_digest,
            code_verifier, native_redirect_uri,
            created_at, expires_at, consumed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            login.transaction_digest,
            login.state_digest,
            login.nonce_digest,
            login.code_verifier,
            login.native_redirect_uri,
            timestamp(login.created_at),
            timestamp(login.expires_at),
            login.consumed_at.map(timestamp),
        ],
    )?;
    Ok(())
}

pub fn consume_login_transaction(
    tx: &Transaction<'_>,
    state_digest: &str,
    consumed_at: DateTime<Utc>,
) -> Result<Option<LoginTransactionRecord>> {
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
                code_verifier, native_redirect_uri,
                created_at, expires_at, consumed_at
         FROM auth_login_transactions
         WHERE state_digest = ?1",
        [state_digest],
        |row| {
            Ok(LoginTransactionRecord {
                transaction_digest: row.get(0)?,
                state_digest: row.get(1)?,
                nonce_digest: row.get(2)?,
                code_verifier: row.get(3)?,
                native_redirect_uri: row.get(4)?,
                created_at: parse_timestamp(row.get(5)?)?,
                expires_at: parse_timestamp(row.get(6)?)?,
                consumed_at: row
                    .get::<_, Option<String>>(7)?
                    .map(parse_timestamp)
                    .transpose()?,
            })
        },
    )
    .map(Some)
    .context("reading consumed login transaction")
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
        expires_at: row
            .get::<_, Option<String>>(4)?
            .map(parse_timestamp)
            .transpose()?,
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
mod tests;
