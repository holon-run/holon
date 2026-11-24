use super::*;
use crate::tests::migrated_connection;

#[test]
fn native_bootstrap_proof_is_checked_before_single_use_consumption() -> Result<()> {
    let mut connection = migrated_connection()?;
    let tx = connection.transaction()?;
    let now = Utc::now();
    insert_bootstrap_credential(
        &tx,
        &BootstrapCredentialRecord {
            credential_digest: "native-ticket".into(),
            user_id: None,
            scope: "native-session:correct-challenge".into(),
            created_at: now,
            expires_at: now + chrono::Duration::minutes(2),
            consumed_at: None,
            revoked_at: None,
        },
    )?;
    for scopes in [
        &["session", "recovery"][..],
        &["native-session:wrong-challenge"][..],
    ] {
        assert!(
            consume_bootstrap_credential_for_scopes(&tx, "native-ticket", now, Some(scopes))?
                .is_none()
        );
    }
    assert!(consume_bootstrap_credential_for_scopes(
        &tx,
        "native-ticket",
        now,
        Some(&["native-session:correct-challenge"])
    )?
    .is_some());
    assert!(consume_bootstrap_credential_for_scopes(
        &tx,
        "native-ticket",
        now,
        Some(&["native-session:correct-challenge"])
    )?
    .is_none());
    Ok(())
}

#[test]
fn bootstrap_credential_is_consumed_once() -> Result<()> {
    let mut connection = migrated_connection()?;
    let tx = connection.transaction()?;
    let now = Utc::now();
    insert_bootstrap_credential(
        &tx,
        &BootstrapCredentialRecord {
            credential_digest: "digest".into(),
            user_id: None,
            scope: "session_exchange".into(),
            created_at: now,
            expires_at: now + chrono::Duration::hours(1),
            consumed_at: None,
            revoked_at: None,
        },
    )?;

    assert!(consume_bootstrap_credential_for_scopes(&tx, "digest", now, None)?.is_some());
    assert!(consume_bootstrap_credential_for_scopes(&tx, "digest", now, None)?.is_none());
    Ok(())
}

#[test]
fn touching_session_does_not_move_last_seen_backwards() -> Result<()> {
    let mut connection = migrated_connection()?;
    let tx = connection.transaction()?;
    let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis())
        .expect("valid timestamp");
    upsert_user(
        &tx,
        &AuthUserRecord {
            user_id: "user".into(),
            issuer: "local".into(),
            subject: "user".into(),
            display_name: None,
            email: None,
            created_at: now,
            updated_at: now,
            disabled_at: None,
        },
    )?;
    create_session(
        &tx,
        &AuthSessionRecord {
            session_digest: "session".into(),
            user_id: "user".into(),
            auth_method: "local".into(),
            created_at: now,
            expires_at: Some(now + chrono::Duration::hours(1)),
            last_seen_at: now + chrono::Duration::minutes(5),
            revoked_at: None,
        },
    )?;

    assert!(!touch_session(&tx, "session", now)?);
    assert_eq!(
        find_session(&tx, "session")?
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
    let mut connection = migrated_connection()?;
    let tx = connection.transaction()?;
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
    upsert_user(&tx, &user)?;

    let mut rebound = user;
    rebound.subject = "subject-b".into();
    let error = upsert_user(&tx, &rebound).unwrap_err();
    assert!(error.to_string().contains("already bound"));
    Ok(())
}
