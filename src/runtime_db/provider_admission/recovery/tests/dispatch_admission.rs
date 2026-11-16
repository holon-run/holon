use super::*;

mod atomic;

fn incident(db: &RuntimeDb, scope: &ProviderAdmissionScope) -> Result<DateTime<Utc>> {
    let admission = lease(db, std::slice::from_ref(scope))?;
    assert!(db.record_provider_admission_failure(
        &admission,
        scope,
        ProviderCooldownPolicy::Transport,
        now(),
        None,
        "test/model",
    )?);
    Ok(db
        .provider_admission_blocked(std::slice::from_ref(scope), now())?
        .context("expected cooldown")?
        .not_before)
}

fn reserve_at(
    db: &RuntimeDb,
    scopes: &[ProviderAdmissionScope],
    at: DateTime<Utc>,
) -> Result<ProviderAdmissionLease> {
    match db.reserve_provider_admission(scopes, at, Duration::minutes(2))? {
        ProviderAdmissionReservation::Granted(lease) => Ok(lease),
        other => anyhow::bail!("expected reservation: {other:?}"),
    }
}

fn admit(
    db: &RuntimeDb,
    episode: &ProviderRecoveryEpisode,
    scopes: &[ProviderAdmissionScope],
    request_id: &str,
    attempt: &ExecutionAttempt,
    at: DateTime<Utc>,
) -> Result<ProviderRecoveryDispatchAdmission> {
    db.admit_provider_recovery_dispatch_for_attempt(
        episode,
        scopes,
        request_id,
        2,
        attempt,
        at,
        Duration::minutes(2),
        true,
    )
}

type ScopeSnapshot = (i64, i64, Option<String>, Option<String>);

fn snapshot(db: &RuntimeDb, scopes: &[ProviderAdmissionScope]) -> Result<Vec<ScopeSnapshot>> {
    db.with_read(|connection| {
        scopes
            .iter()
            .map(|scope| {
                Ok(connection.query_row(
                    &format!(
                        "SELECT revision, consecutive_failures, probe_token, probe_lease_until
                         FROM {} WHERE identity_key = ?1",
                        scope.table()
                    ),
                    [scope.key()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?)
            })
            .collect()
    })
}

#[test]
fn cooldown_blocks_admission_without_charging_or_touching_scope_state() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let episode = canonical_episode(&db, &attempt)?;
    let scope = transport("shared-path");
    let scopes = [scope.clone()];
    let _old = lease(&db, &scopes)?;
    let due = incident(&db, &scope)?;
    let before = snapshot(&db, &scopes)?;
    let ProviderRecoveryDispatchAdmission::Blocked(blocked) =
        admit(&db, &episode, &scopes, "fresh-request", &attempt, now())?
    else {
        anyhow::bail!("cooldown must block admission");
    };
    assert_eq!(blocked.scope, scope);
    assert_eq!(blocked.not_before, due);
    assert_eq!(snapshot(&db, &scopes)?, before);
    assert_not_charged(&db, &episode)?;
    Ok(())
}

#[test]
fn concurrent_probe_blocks_admission_until_the_active_lease_expires() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let episode = canonical_episode(&db, &attempt)?;
    let scopes = [transport("concurrent-probe")];
    let due = incident(&db, &scopes[0])?;
    let holder = reserve_at(&db, &scopes, due)?;
    let before = snapshot(&db, &scopes)?;
    let ProviderRecoveryDispatchAdmission::Blocked(blocked) =
        admit(&db, &episode, &scopes, "fresh-request", &attempt, due)?
    else {
        anyhow::bail!("an active foreign probe must block admission");
    };
    assert_eq!(blocked.scope, scopes[0]);
    assert_eq!(blocked.not_before, due + Duration::minutes(2));
    assert_eq!(snapshot(&db, &scopes)?, before);
    assert!(db.validate_provider_admission(&holder, due)?);
    assert_not_charged(&db, &episode)?;
    Ok(())
}

#[test]
fn admission_after_probe_expiry_charges_a_fresh_probe_atomically() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let episode = canonical_episode(&db, &attempt)?;
    let scopes = [transport("expired-probe")];
    let due = incident(&db, &scopes[0])?;
    let old = reserve_at(&db, &scopes, due)?;
    let later = due + Duration::minutes(2);
    let ProviderRecoveryDispatchAdmission::Charged { lease, dispatch } =
        admit(&db, &episode, &scopes, "fresh-request", &attempt, later)?
    else {
        anyhow::bail!("an expired probe should be re-admitted atomically");
    };
    assert!(lease.is_probe());
    assert_ne!(lease.scopes[0].probe_token, old.scopes[0].probe_token);
    assert!(db.validate_provider_admission(&lease, later)?);
    db.release_provider_admission(&old)?;
    assert_eq!(db.record_provider_admission_success(&old, later)?, 0);
    assert!(db.validate_provider_admission(&lease, later)?);
    assert_eq!(db.record_provider_admission_success(&lease, later)?, 1);
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::AvailableResponse,
        false,
        later,
    )?);
    let current = refresh(&db, &episode)?;
    assert_eq!((current.request_count, current.total_requests), (1, 1));
    assert_eq!(current.candidate_cursor, 2);
    Ok(())
}

#[test]
fn healthy_scope_is_admitted_without_a_probe() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let episode = canonical_episode(&db, &attempt)?;
    let scopes = [transport("healthy-again")];
    let _old = lease(&db, &scopes)?;
    let due = incident(&db, &scopes[0])?;
    let probe = reserve_at(&db, &scopes, due)?;
    assert_eq!(db.record_provider_admission_success(&probe, due)?, 1);
    let ProviderRecoveryDispatchAdmission::Charged { lease, .. } =
        admit(&db, &episode, &scopes, "fresh-request", &attempt, due)?
    else {
        anyhow::bail!("a healthy revision should admit without a probe");
    };
    assert!(!lease.is_probe());
    assert!(db.validate_provider_admission(&lease, due)?);
    let current = refresh(&db, &episode)?;
    assert_eq!((current.request_count, current.total_requests), (1, 1));
    Ok(())
}

#[test]
fn network_preview_is_read_only_and_matches_admission_semantics() -> Result<()> {
    let (_dir, db) = database()?;
    let network = episode(&db, ProviderRecoveryBudgetKind::Network)?;

    // A never-failed scope needs no probe and must not grow an incident row.
    let fresh = [transport("preview-fresh")];
    assert!(matches!(
        db.preview_provider_recovery_admission(&network, &fresh, now())?,
        ProviderRecoveryAdmissionPreview::Granted { is_probe: false }
    ));
    db.with_read(|connection| {
        let rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM provider_transport_incidents WHERE identity_key = ?1",
            [fresh[0].key()],
            |row| row.get(0),
        )?;
        ensure!(rows == 0, "preview must not persist admission rows");
        Ok(())
    })?;
    assert_eq!(refresh(&db, &network)?.revision, network.revision);

    // Network policy cannot skip a local transport cooldown.
    let cooling = [transport("preview-cooling")];
    let due = incident(&db, &cooling[0])?;
    let before = snapshot(&db, &cooling)?;
    match db.preview_provider_recovery_admission(&network, &cooling, now())? {
        ProviderRecoveryAdmissionPreview::Blocked(blocked) => {
            assert_eq!(blocked.not_before, due);
        }
        other => anyhow::bail!("network cooldown must block: {other:?}"),
    }
    assert_eq!(snapshot(&db, &cooling)?, before);

    // The episode fence and attempted-scope screen are plain denials.
    let attempted = [transport("preview-attempted")];
    let holder = lease(&db, &attempted)?;
    let dispatch = charge(&db, &network, &holder, "preview-charged", 1)?;
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::NetworkFailure(&attempted[0]),
        false,
        now(),
    )?);
    let network = refresh(&db, &network)?;
    assert_eq!(
        db.preview_provider_recovery_admission(&network, &attempted, now())?,
        ProviderRecoveryAdmissionPreview::Denied(
            ProviderRecoveryDispatchDenial::ScopeAlreadyAttempted
        )
    );
    let mut stale = refresh(&db, &network)?;
    stale.revision += 1;
    assert_eq!(
        db.preview_provider_recovery_admission(&stale, &fresh, now())?,
        ProviderRecoveryAdmissionPreview::Denied(ProviderRecoveryDispatchDenial::StaleEpisode)
    );
    Ok(())
}

#[test]
fn operator_preview_ignores_local_cooldown_but_never_a_server_floor() -> Result<()> {
    let (_dir, db) = database()?;
    let operator = episode(&db, ProviderRecoveryBudgetKind::Operator)?;

    // Operator policy may ignore a local transient cooldown, but not a
    // server floor, and still reports the probe it would reserve.
    let local = quota("preview-quota-local");
    let local_scopes = [local.clone()];
    let admission = lease(&db, &local_scopes)?;
    assert!(db.record_provider_admission_failure(
        &admission,
        &local,
        ProviderCooldownPolicy::TransientQuota,
        now(),
        None,
        "test/model",
    )?);
    let quota_before = snapshot(&db, &local_scopes)?;
    assert!(matches!(
        db.preview_provider_recovery_admission(&operator, &local_scopes, now())?,
        ProviderRecoveryAdmissionPreview::Granted { is_probe: true }
    ));

    let floor = now() + Duration::minutes(5);
    let floored = quota("preview-quota-floor");
    let floored_scopes = [floored.clone()];
    let floor_admission = lease(&db, &floored_scopes)?;
    assert!(db.record_provider_admission_failure(
        &floor_admission,
        &floored,
        ProviderCooldownPolicy::TransientQuota,
        now(),
        Some(floor),
        "test/model",
    )?);
    match db.preview_provider_recovery_admission(&operator, &floored_scopes, now())? {
        ProviderRecoveryAdmissionPreview::Blocked(blocked) => {
            assert_eq!(blocked.not_before, floor);
        }
        other => anyhow::bail!("operator policy cannot skip a server floor: {other:?}"),
    }
    assert_eq!(snapshot(&db, &local_scopes)?, quota_before);
    let floored_state = snapshot(&db, &floored_scopes)?;
    assert_eq!(floored_state[0].2, None);
    assert_eq!(floored_state[0].3, None);
    assert_eq!(refresh(&db, &operator)?.revision, operator.revision);
    Ok(())
}

#[test]
fn resolved_preview_ignores_old_epoch_budget_without_reopening_admission() -> Result<()> {
    let (_dir, db) = database()?;
    let attempt = canonical_root(&db)?;
    let mut current = canonical_episode(&db, &attempt)?;
    let scopes = [transport("resolved-preview-0")];
    for index in 0..MAX_REQUESTS {
        let scope = transport(&format!("resolved-preview-{index}"));
        let holder = lease(&db, std::slice::from_ref(&scope))?;
        let dispatch = charge(&db, &current, &holder, &format!("request-{index}"), index)?;
        let last = index + 1 == MAX_REQUESTS;
        if last {
            current = refresh(&db, &current)?;
            assert_eq!(
                db.preview_provider_recovery_admission(&current, &scopes, now())?,
                ProviderRecoveryAdmissionPreview::Denied(
                    ProviderRecoveryDispatchDenial::BudgetExhausted
                )
            );
        }
        assert!(db.record_provider_recovery_request_outcome(
            &dispatch,
            if last {
                ProviderRecoveryRequestOutcome::AvailableResponse
            } else {
                ProviderRecoveryRequestOutcome::NetworkFailure(&scope)
            },
            false,
            now(),
        )?);
        current = refresh(&db, &current)?;
    }
    assert_eq!(current.state, ProviderRecoveryBudgetState::Resolved);
    assert!(db.provider_recovery_candidate_attempted(&current, &scopes)?);
    let before = snapshot(&db, &scopes)?;
    assert_eq!(
        db.preview_provider_recovery_admission(&current, &scopes, now())?,
        ProviderRecoveryAdmissionPreview::Granted { is_probe: false }
    );
    let unchanged = refresh(&db, &current)?;
    assert_eq!(unchanged.state, ProviderRecoveryBudgetState::Resolved);
    assert_eq!(unchanged.epoch, current.epoch);
    assert_eq!(unchanged.revision, current.revision);
    assert_eq!(unchanged.request_count, MAX_REQUESTS);
    assert_eq!(unchanged.total_requests, i64::from(MAX_REQUESTS));
    assert_eq!(snapshot(&db, &scopes)?, before);
    assert!(matches!(
        admit(&db, &current, &scopes, "without-epoch", &attempt, now())?,
        ProviderRecoveryDispatchAdmission::Denied(ProviderRecoveryDispatchDenial::BudgetExhausted)
    ));
    let next = db
        .begin_provider_recovery_probe_epoch(&current, now())?
        .context("send point must explicitly begin a new epoch")?;
    assert!(matches!(
        admit(&db, &next, &scopes, "next-epoch", &attempt, now())?,
        ProviderRecoveryDispatchAdmission::Charged { .. }
    ));
    Ok(())
}

#[test]
fn resolved_operator_preview_uses_network_cooldown_and_never_reserves_a_probe() -> Result<()> {
    let (_dir, db) = database()?;
    let original = episode(&db, ProviderRecoveryBudgetKind::Operator)?;
    let healthy = [transport("resolved-operator")];
    let holder = lease(&db, &healthy)?;
    let dispatch = charge(&db, &original, &holder, "resolved-operator", 1)?;
    assert!(db.record_provider_recovery_request_outcome(
        &dispatch,
        ProviderRecoveryRequestOutcome::AvailableResponse,
        false,
        now(),
    )?);
    let resolved = refresh(&db, &original)?;
    assert_eq!(resolved.state, ProviderRecoveryBudgetState::Resolved);
    assert_eq!(resolved.kind, ProviderRecoveryBudgetKind::Operator);
    assert_eq!(
        db.preview_provider_recovery_admission(&original, &healthy, now())?,
        ProviderRecoveryAdmissionPreview::Denied(ProviderRecoveryDispatchDenial::StaleEpisode)
    );
    assert_denied(
        db.charge_provider_recovery_dispatch(
            &resolved,
            &holder,
            "resolved-without-epoch",
            1,
            now(),
        )?,
        ProviderRecoveryDispatchDenial::EpisodePaused,
    );

    let scopes = [transport("resolved-operator-cooling")];
    let due = incident(&db, &scopes[0])?;
    let before = snapshot(&db, &scopes)?;
    assert!(matches!(
        db.preview_provider_recovery_admission(&resolved, &scopes, now())?,
        ProviderRecoveryAdmissionPreview::Blocked(_)
    ));
    assert_eq!(
        db.preview_provider_recovery_admission(&resolved, &scopes, due)?,
        ProviderRecoveryAdmissionPreview::Granted { is_probe: true }
    );
    assert_eq!(snapshot(&db, &scopes)?, before);

    let probe = reserve_at(&db, &scopes, due)?;
    assert!(probe.is_probe());
    let leased = snapshot(&db, &scopes)?;
    assert!(matches!(
        db.preview_provider_recovery_admission(&resolved, &scopes, due)?,
        ProviderRecoveryAdmissionPreview::Blocked(_)
    ));
    assert_eq!(snapshot(&db, &scopes)?, leased);
    assert_eq!(refresh(&db, &resolved)?.revision, resolved.revision);
    Ok(())
}
