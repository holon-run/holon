use super::*;

#[test]
fn candidate_identity_is_hashed_and_payment_failure_is_isolated() -> Result<()> {
    let (_dir, db) = database()?;
    let raw = "provider/endpoint/credential-secret/model-a";
    let failed = ProviderAdmissionScope::candidate(raw)?;
    assert_eq!(failed, ProviderAdmissionScope::candidate(raw)?);
    assert!(ProviderAdmissionScope::candidate(" ").is_err());
    let path = transport("shared-endpoint");
    let account = quota("shared-account");
    let lease = reserve(&db, &[failed.clone(), path.clone(), account.clone()], now())?;
    for (scope, policy) in [
        (&failed, ProviderCooldownPolicy::Transport),
        (&failed, ProviderCooldownPolicy::HardQuota),
        (&account, ProviderCooldownPolicy::PaymentRequired),
        (&path, ProviderCooldownPolicy::PaymentRequired),
    ] {
        assert!(db
            .record_provider_admission_failure(&lease, scope, policy, now(), None, "test/model")
            .is_err());
    }
    assert!(db.record_provider_admission_failure(
        &lease,
        &failed,
        ProviderCooldownPolicy::PaymentRequired,
        now(),
        None,
        "test/model",
    )?);
    for raw in [
        "provider/endpoint/other-credential/model-a",
        "provider/endpoint/credential-secret/model-b",
        "provider/other-endpoint/credential-secret/model-a",
    ] {
        let healthy = ProviderAdmissionScope::candidate(raw)?;
        assert_ne!(failed, healthy);
        let reservation = reserve(&db, &[healthy, path.clone(), account.clone()], now())?;
        assert!(!reservation.is_probe());
    }
    let stored = serde_json::to_string(&failed)?;
    assert!(!stored.contains("credential-secret"));
    assert!(!stored.contains("endpoint"));
    db.with_read(|connection| {
        let row: (String, String, String) = connection.query_row(
            "SELECT identity_key, last_failure_kind, cooldown_policy
             FROM provider_candidate_incidents",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(row.0, failed.key());
        assert_eq!(row.1, "payment_required");
        assert_eq!(row.2, "payment_required");
        Ok(())
    })?;
    Ok(())
}

#[test]
fn payment_backoff_survives_restart_and_is_capped() -> Result<()> {
    let (dir, mut db) = database()?;
    let scope = ProviderAdmissionScope::candidate("provider/endpoint/credential/model")?;
    let mut at = now();
    for failures in 1..=8 {
        let due = fail(&db, &scope, at, ProviderCooldownPolicy::PaymentRequired)?;
        let base = Duration::minutes(15 * (1_i64 << (failures - 1))).min(Duration::hours(6));
        assert!(due - at >= base);
        assert!(due - at <= (base + base / 10).min(Duration::hours(6)));
        drop(db);
        db = reopen(&dir)?;
        let before_due = due - Duration::milliseconds(1);
        assert_eq!(
            db.provider_admission_blocked(std::slice::from_ref(&scope), before_due)?
                .unwrap()
                .not_before,
            due
        );
        assert!(matches!(
            db.reserve_provider_admission(
                std::slice::from_ref(&scope),
                before_due,
                Duration::minutes(2),
            )?,
            ProviderAdmissionReservation::Blocked(_)
        ));
        at = due;
    }
    Ok(())
}

#[test]
fn payment_probe_is_single_flight_and_stale_results_cannot_settle_replacement() -> Result<()> {
    let (dir, db) = database()?;
    let scope = ProviderAdmissionScope::candidate("candidate-a")?;
    let due = fail(&db, &scope, now(), ProviderCooldownPolicy::PaymentRequired)?;
    let old = reserve(&db, std::slice::from_ref(&scope), due)?;
    assert!(old.is_probe());
    let other = reopen(&dir)?;
    assert!(matches!(
        other.reserve_provider_admission(
            std::slice::from_ref(&scope),
            due,
            Duration::minutes(2),
        )?,
        ProviderAdmissionReservation::Blocked(_)
    ));
    let at = due + Duration::minutes(2);
    let replacement = reserve(&other, std::slice::from_ref(&scope), at)?;
    assert!(!db.validate_provider_admission(&old, at)?);
    assert_eq!(db.record_provider_admission_success(&old, at)?, 0);
    assert!(!db.record_provider_admission_failure(
        &old,
        &scope,
        ProviderCooldownPolicy::PaymentRequired,
        at,
        None,
        "a",
    )?);
    assert!(other.validate_provider_admission(&replacement, at)?);
    assert!(other.record_provider_admission_failure(
        &replacement,
        &scope,
        ProviderCooldownPolicy::PaymentRequired,
        at,
        None,
        "a",
    )?);
    let next = other
        .provider_admission_blocked(std::slice::from_ref(&scope), at)?
        .unwrap()
        .not_before;
    assert!(next - at >= Duration::minutes(30));
    let release = reserve(&db, std::slice::from_ref(&scope), next)?;
    db.release_provider_admission(&release)?;
    let success = reserve(&other, std::slice::from_ref(&scope), next)?;
    assert_eq!(other.record_provider_admission_success(&success, next)?, 1);
    assert_eq!(db.record_provider_admission_success(&release, next)?, 0);
    assert!(db
        .provider_admission_blocked(std::slice::from_ref(&scope), next)?
        .is_none());
    Ok(())
}

#[test]
fn payment_server_deadline_and_success_are_scope_specific() -> Result<()> {
    let (dir, db) = database()?;
    let a = ProviderAdmissionScope::candidate("candidate-a")?;
    let b = ProviderAdmissionScope::candidate("candidate-b")?;
    let server = now() + Duration::hours(12);
    let initial = reserve(&db, std::slice::from_ref(&a), now())?;
    assert!(db.record_provider_admission_failure(
        &initial,
        &a,
        ProviderCooldownPolicy::PaymentRequired,
        now(),
        Some(server),
        "a",
    )?);
    let due = db
        .provider_admission_blocked(std::slice::from_ref(&a), now())?
        .unwrap()
        .not_before;
    assert!(due > server);
    let b_due = fail(&db, &b, now(), ProviderCooldownPolicy::PaymentRequired)?;
    let other = reopen(&dir)?;
    let healthy = reserve(&other, std::slice::from_ref(&b), b_due)?;
    assert_eq!(other.record_provider_admission_success(&healthy, b_due)?, 1);
    assert_eq!(
        db.provider_admission_blocked(std::slice::from_ref(&a), b_due)?
            .unwrap()
            .not_before,
        due
    );
    Ok(())
}
