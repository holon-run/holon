use super::*;

#[test]
fn payment_outcome_keeps_healthy_shared_quota_available_after_restart() -> Result<()> {
    for kind in [
        ProviderRecoveryBudgetKind::Network,
        ProviderRecoveryBudgetKind::Operator,
    ] {
        let (dir, db) = database()?;
        let current = episode(&db, kind)?;
        let a = ProviderAdmissionScope::candidate("candidate-a")?;
        let b = ProviderAdmissionScope::candidate("candidate-b")?;
        let shared = [quota("account"), transport("shared")];
        let scopes_a = [shared[0].clone(), shared[1].clone(), a.clone()];
        let scopes_b = [shared[0].clone(), shared[1].clone(), b];
        let reservation = lease(&db, &scopes_a)?;
        let dispatch = charge(&db, &current, &reservation, "payment-a", 1)?;
        assert!(db.record_provider_admission_failure(
            &reservation,
            &a,
            ProviderCooldownPolicy::PaymentRequired,
            now(),
            None,
            "a",
        )?);
        assert!(db.record_provider_recovery_request_outcome(
            &dispatch,
            ProviderRecoveryRequestOutcome::PaymentRequired(&a),
            false,
            now(),
        )?);
        let reopened = reopen(&dir)?;
        let current = refresh(&reopened, &current)?;
        assert!(reopened.provider_recovery_candidate_attempted(&current, &scopes_a)?);
        assert!(!reopened.provider_recovery_candidate_attempted(&current, &scopes_b)?);
        let ProviderRecoveryAdmissionReservation::Granted(reservation) = reopened
            .reserve_provider_recovery_admission(
                &current,
                &scopes_b,
                now(),
                Duration::minutes(2),
            )?
        else {
            anyhow::bail!("healthy model sharing the credential was excluded");
        };
        charge(&reopened, &current, &reservation, "healthy-b", 2)?;
        assert_eq!(refresh(&reopened, &current)?.request_count, 2);
    }
    Ok(())
}

#[test]
fn payment_outcome_does_not_restore_a_consumed_operator_probe() -> Result<()> {
    let (_dir, db) = database()?;
    let shared = transport("shared");
    let initial = lease(&db, std::slice::from_ref(&shared))?;
    assert!(db.record_provider_admission_failure(
        &initial,
        &shared,
        ProviderCooldownPolicy::Transport,
        now(),
        None,
        "initial",
    )?);
    let current = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    let candidate = ProviderAdmissionScope::candidate("candidate-a")?;
    let account = quota("healthy-account");
    let ProviderRecoveryAdmissionReservation::Granted(probe) = db
        .reserve_provider_recovery_admission(
            &current,
            &[shared.clone(), candidate.clone(), account.clone()],
            now(),
            Duration::minutes(2),
        )?
    else {
        anyhow::bail!("expected the operator transport probe");
    };
    assert!(probe.is_probe());
    let dispatch = charge(&db, &current, &probe, "payment-probe", 1)?;
    assert!(db.record_provider_admission_failure(
        &probe,
        &candidate,
        ProviderCooldownPolicy::PaymentRequired,
        now(),
        None,
        "a",
    )?);
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::PaymentRequired(&candidate),
        false,
        now(),
    )?);
    let current = refresh(&db, &current)?;
    assert!(db.provider_recovery_candidate_attempted(&current, &[shared])?);
    assert!(!db.provider_recovery_candidate_attempted(&current, &[account])?);
    Ok(())
}

#[test]
fn payment_outcome_requires_an_admitted_candidate_scope() -> Result<()> {
    let (_dir, db) = database()?;
    let current = episode(&db, ProviderRecoveryBudgetKind::Network)?;
    let candidate = ProviderAdmissionScope::candidate("candidate-a")?;
    let path = transport("shared");
    let reservation = lease(&db, &[candidate.clone(), path.clone()])?;
    let dispatch = charge(&db, &current, &reservation, "payment", 1)?;
    for invalid in [
        path,
        quota("account"),
        ProviderAdmissionScope::candidate("foreign")?,
    ] {
        assert!(db
            .record_provider_recovery_request_outcome(
                &dispatch,
                ProviderRecoveryRequestOutcome::PaymentRequired(&invalid),
                false,
                now(),
            )
            .is_err());
    }
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::PaymentRequired(&candidate),
        false,
        now(),
    )?);
    assert!(!db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::AvailableResponse,
        false,
        now(),
    )?);
    assert_eq!(refresh(&db, &current)?.request_count, 1);
    Ok(())
}

#[test]
fn payment_deadline_is_not_operator_overridable_and_epochs_are_fenced() -> Result<()> {
    for kind in [
        ProviderRecoveryBudgetKind::Network,
        ProviderRecoveryBudgetKind::Operator,
    ] {
        let (dir, db) = database()?;
        let candidate = ProviderAdmissionScope::candidate("candidate-a")?;
        let initial = lease(&db, std::slice::from_ref(&candidate))?;
        assert!(db.record_provider_admission_failure(
            &initial,
            &candidate,
            ProviderCooldownPolicy::PaymentRequired,
            now(),
            None,
            "a",
        )?);
        let due = db
            .provider_admission_blocked(std::slice::from_ref(&candidate), now())?
            .unwrap()
            .not_before;
        let mut current = episode(&db, kind)?;
        for at in [now(), due - Duration::milliseconds(1)] {
            assert!(matches!(
                db.reserve_provider_recovery_admission(
                    &current,
                    std::slice::from_ref(&candidate),
                    at,
                    Duration::minutes(2),
                )?,
                ProviderRecoveryAdmissionReservation::Blocked(_)
            ));
            assert_eq!(refresh(&db, &current)?.request_count, 0);
        }
        let ProviderRecoveryAdmissionReservation::Granted(probe) = db
            .reserve_provider_recovery_admission(
                &current,
                std::slice::from_ref(&candidate),
                due,
                Duration::minutes(2),
            )?
        else {
            anyhow::bail!("expected due payment probe")
        };
        assert!(probe.is_probe());
        let ProviderRecoveryDispatchResult::Charged(dispatch) =
            db.charge_provider_recovery_dispatch(&current, &probe, "payment-probe", 1, due)?
        else {
            anyhow::bail!("expected charged payment probe")
        };
        assert!(db.record_provider_admission_failure(
            &probe,
            &candidate,
            ProviderCooldownPolicy::PaymentRequired,
            due,
            None,
            "a",
        )?);
        assert!(db.record_provider_recovery_request_outcome(
            &dispatch,
            ProviderRecoveryRequestOutcome::PaymentRequired(&candidate),
            false,
            due,
        )?);
        current = refresh(&db, &current)?;
        let next = db
            .provider_admission_blocked(std::slice::from_ref(&candidate), due)?
            .unwrap()
            .not_before;
        assert!(next - due >= Duration::minutes(30));
        assert!(db.park_provider_recovery_episode(&current, next, false, due)?);
        let parked = refresh(&db, &current)?;
        assert_eq!(parked.state, ProviderRecoveryBudgetState::Cooling);
        assert_eq!((parked.request_count, parked.total_requests), (1, 1));
        let other = reopen(&dir)?;
        for _ in 0..3 {
            let reopened = episode(&other, kind)?;
            assert_eq!(
                (
                    reopened.epoch,
                    reopened.request_count,
                    reopened.total_requests,
                    reopened.not_before
                ),
                (parked.epoch, 1, 1, parked.not_before)
            );
            assert!(other
                .begin_provider_recovery_probe_epoch(&reopened, next - Duration::milliseconds(1))?
                .is_none());
        }
        let fresh = other
            .begin_provider_recovery_probe_epoch(&parked, next)?
            .context("expected one due epoch")?;
        assert!(db
            .begin_provider_recovery_probe_epoch(&parked, next)?
            .is_none());
        assert_eq!(
            (fresh.epoch, fresh.request_count, fresh.total_requests),
            (2, 0, 1)
        );
        assert_eq!(fresh.candidate_cursor, parked.candidate_cursor);
        assert_eq!(fresh.kind, ProviderRecoveryBudgetKind::Network);
        for outcome in [
            ProviderRecoveryRequestOutcome::AvailableResponse,
            ProviderRecoveryRequestOutcome::PaymentRequired(&candidate),
        ] {
            assert!(!db.record_provider_recovery_request_outcome(&dispatch, outcome, false, next,)?);
        }
        let after = refresh(&db, &fresh)?;
        assert_eq!(
            (
                after.revision,
                after.request_count,
                after.total_requests,
                after.not_before
            ),
            (fresh.revision, 0, 1, fresh.not_before)
        );
    }
    Ok(())
}
