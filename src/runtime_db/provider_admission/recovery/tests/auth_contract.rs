use super::*;

#[test]
fn auth_failure_isolates_credential_scope_and_keeps_episode_unresolved() -> Result<()> {
    for kind in [
        ProviderRecoveryBudgetKind::Network,
        ProviderRecoveryBudgetKind::Operator,
    ] {
        let (dir, db) = database()?;
        let current = episode(&db, kind)?;
        let dead_credential = quota("dead-credential");
        let healthy_credential = quota("healthy-credential");
        let shared_path = transport("shared");
        let candidate_a = ProviderAdmissionScope::candidate("candidate-a")?;
        let candidate_b = ProviderAdmissionScope::candidate("candidate-b")?;
        let scopes_a = [
            dead_credential.clone(),
            shared_path.clone(),
            candidate_a.clone(),
        ];
        let scopes_b = [healthy_credential.clone(), shared_path, candidate_b];
        let reservation = lease(&db, &scopes_a)?;
        let dispatch = charge(&db, &current, &reservation, "auth-a", 1)?;
        assert!(db.record_provider_admission_failure(
            &reservation,
            &dead_credential,
            ProviderCooldownPolicy::Credential,
            now(),
            None,
            "a",
        )?);
        assert!(db.record_provider_recovery_request_outcome(
            &dispatch,
            ProviderRecoveryRequestOutcome::AuthFailure(&dead_credential),
            false,
            now(),
        )?);
        let current = refresh(&db, &current)?;
        assert_ne!(
            current.state,
            ProviderRecoveryBudgetState::Resolved,
            "deterministic auth failure must not resolve the episode"
        );
        assert!(db.provider_recovery_candidate_attempted(&current, &scopes_a)?);
        assert!(!db.provider_recovery_candidate_attempted(&current, &scopes_b)?);
        // The failed credential keeps a persistent cooldown, and the epoch's
        // attempted fence excludes the same scopes from re-admission; a
        // different credential stays healthy.
        let blocked = db.provider_admission_blocked(&[dead_credential.clone()], now())?;
        let blocked = blocked.expect("auth failure must persist a credential cooldown");
        assert!(blocked.not_before > now() + Duration::minutes(14));
        assert!(matches!(
            db.reserve_provider_recovery_admission(
                &current,
                &scopes_a,
                now(),
                Duration::minutes(2),
            )?,
            ProviderRecoveryAdmissionReservation::Denied(_)
        ));
        let ProviderRecoveryAdmissionReservation::Granted(reservation) = db
            .reserve_provider_recovery_admission(
                &current,
                &scopes_b,
                now(),
                Duration::minutes(2),
            )?
        else {
            anyhow::bail!("healthy credential was excluded by a foreign auth failure");
        };
        charge(&db, &current, &reservation, "healthy-b", 2)?;
        assert_eq!(refresh(&db, &current)?.request_count, 2);

        let reopened = reopen(&dir)?;
        let current = refresh(&reopened, &current)?;
        assert_ne!(current.state, ProviderRecoveryBudgetState::Resolved);
        assert!(reopened.provider_recovery_candidate_attempted(&current, &scopes_a)?);
    }
    Ok(())
}

#[test]
fn contract_failure_isolates_candidate_scope_and_requires_admitted_scope() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let credential = quota("account");
    let candidate = ProviderAdmissionScope::candidate("candidate-a")?;
    let path = transport("shared");
    let scopes = [credential.clone(), path.clone(), candidate.clone()];
    let reservation = lease(&db, &scopes)?;
    let dispatch = charge(&db, &current, &reservation, "contract-a", 1)?;
    assert!(db.record_provider_admission_failure(
        &reservation,
        &candidate,
        ProviderCooldownPolicy::Contract,
        now(),
        None,
        "a",
    )?);
    for invalid in [
        path,
        credential,
        ProviderAdmissionScope::candidate("foreign")?,
    ] {
        assert!(db
            .record_provider_recovery_request_outcome(
                &dispatch,
                ProviderRecoveryRequestOutcome::ContractFailure(&invalid),
                false,
                now(),
            )
            .is_err());
    }
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::ContractFailure(&candidate),
        false,
        now(),
    )?);
    let current = refresh(&db, &current)?;
    assert_ne!(current.state, ProviderRecoveryBudgetState::Resolved);
    assert!(db.provider_recovery_candidate_attempted(&current, &scopes)?);
    let blocked = db.provider_admission_blocked(&[candidate.clone()], now())?;
    let blocked = blocked.expect("contract failure must persist a candidate cooldown");
    assert!(blocked.not_before > now() + Duration::minutes(14));
    assert!(matches!(
        db.reserve_provider_recovery_admission(&current, &scopes, now(), Duration::minutes(2),)?,
        ProviderRecoveryAdmissionReservation::Denied(_)
    ));
    Ok(())
}

#[test]
fn auth_failure_rejects_transport_and_foreign_scopes() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let credential = quota("account");
    let candidate = ProviderAdmissionScope::candidate("candidate-a")?;
    let path = transport("shared");
    let scopes = [credential.clone(), path.clone(), candidate];
    let reservation = lease(&db, &scopes)?;
    let dispatch = charge(&db, &current, &reservation, "auth-a", 1)?;
    for invalid in [path, ProviderAdmissionScope::candidate("foreign")?] {
        assert!(db
            .record_provider_recovery_request_outcome(
                &dispatch,
                ProviderRecoveryRequestOutcome::AuthFailure(&invalid),
                false,
                now(),
            )
            .is_err());
    }
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::AuthFailure(&credential),
        false,
        now(),
    )?);
    assert_ne!(
        refresh(&db, &current)?.state,
        ProviderRecoveryBudgetState::Resolved
    );
    Ok(())
}

#[test]
fn deterministic_policies_are_bound_to_their_scope_kinds() -> Result<()> {
    let (_dir, db) = database()?;
    let credential = quota("account");
    let candidate = ProviderAdmissionScope::candidate("candidate-a")?;
    let reservation = lease(&db, &[credential.clone(), candidate.clone()])?;
    for (scope, policy) in [
        (&credential, ProviderCooldownPolicy::Contract),
        (&candidate, ProviderCooldownPolicy::Credential),
    ] {
        assert!(db
            .record_provider_admission_failure(&reservation, scope, policy, now(), None, "a",)
            .is_err());
    }
    assert!(db.record_provider_admission_failure(
        &reservation,
        &credential,
        ProviderCooldownPolicy::Credential,
        now(),
        None,
        "a",
    )?);
    Ok(())
}
