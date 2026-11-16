use super::*;

#[test]
fn auxiliary_admission_rolls_back_probe_reservations_and_preserves_execution_denials() -> Result<()>
{
    for stale_execution in [false, true] {
        let (_dir, db) = database()?;
        let mut attempt = canonical_root(&db)?;
        let current = canonical_episode(&db, &attempt)?;
        let scopes = [transport("auxiliary-a"), transport("auxiliary-b")];
        let due = incident(&db, &scopes[0])?.max(incident(&db, &scopes[1])?);
        let before = snapshot(&db, &scopes)?;
        if stale_execution {
            attempt.source.generation += 1;
        }
        let result = db.admit_provider_recovery_dispatch_for_attempt(
            &current,
            &scopes,
            "optional-compaction",
            2,
            &attempt,
            due,
            Duration::minutes(2),
            false,
        )?;
        assert_eq!(snapshot(&db, &scopes)?, before);
        assert_not_charged(&db, &current)?;
        if stale_execution {
            assert!(matches!(
                result,
                ProviderRecoveryDispatchAdmission::Denied(
                    ProviderRecoveryDispatchDenial::StaleExecution(_)
                )
            ));
        } else {
            assert!(matches!(
                result,
                ProviderRecoveryDispatchAdmission::AuxiliarySkipped
            ));
            assert!(matches!(
                admit(&db, &current, &scopes, "main-response", &attempt, due)?,
                ProviderRecoveryDispatchAdmission::Charged { .. }
            ));
            assert_eq!(refresh(&db, &current)?.total_requests, 1);
            continue;
        }
    }
    Ok(())
}

#[test]
fn fresh_reservations_roll_back_on_late_denial_and_storage_error() -> Result<()> {
    for case in ["denied", "charge_error", "partial_reservation_error"] {
        let (_dir, db) = database()?;
        let attempt = canonical_root(&db)?;
        let current = canonical_episode(&db, &attempt)?;
        let mut scopes = [transport("probe-a"), transport("probe-b")];
        scopes.sort();
        let due = incident(&db, &scopes[0])?.max(incident(&db, &scopes[1])?);
        let before = snapshot(&db, &scopes)?;
        db.transaction(|tx| {
            match case {
                "denied" => tx.execute_batch(
                    "CREATE TEMP TRIGGER admission_denial
                     AFTER UPDATE OF probe_token ON provider_transport_incidents
                     WHEN NEW.probe_token IS NOT NULL BEGIN
                       UPDATE provider_recovery_episodes SET revision = revision + 1;
                     END",
                )?,
                "charge_error" => tx.execute_batch(
                    "CREATE TEMP TRIGGER admission_charge_error
                     BEFORE INSERT ON provider_recovery_requests BEGIN
                       SELECT RAISE(ABORT, 'injected charge failure');
                     END",
                )?,
                "partial_reservation_error" => tx.execute_batch(&format!(
                    "CREATE TEMP TRIGGER admission_reservation_error
                     BEFORE UPDATE OF probe_token ON provider_transport_incidents
                     WHEN NEW.identity_key = '{}' AND NEW.probe_token IS NOT NULL BEGIN
                       SELECT RAISE(ABORT, 'injected reservation failure');
                     END",
                    scopes[1].key(),
                ))?,
                _ => unreachable!(),
            }
            Ok(())
        })?;
        let result = admit(&db, &current, &scopes, "fresh-request", &attempt, due);
        if case == "denied" {
            assert!(matches!(
                result?,
                ProviderRecoveryDispatchAdmission::Denied(
                    ProviderRecoveryDispatchDenial::StaleEpisode
                )
            ));
        } else {
            assert!(result.is_err(), "{case}");
        }
        assert_eq!(snapshot(&db, &scopes)?, before, "{case}");
        assert_eq!(refresh(&db, &current)?.revision, current.revision, "{case}");
        assert_not_charged(&db, &current)?;
    }
    Ok(())
}

#[test]
fn admission_rechecks_canonical_episode_budget_duplicate_and_attempted_scope() -> Result<()> {
    for case in [
        "execution",
        "episode",
        "budget",
        "paused",
        "duplicate",
        "attempted",
    ] {
        let (_dir, db) = database()?;
        let mut attempt = canonical_root(&db)?;
        let mut current = canonical_episode(&db, &attempt)?;
        let scopes = [transport("admission-fences")];
        let due = incident(&db, &scopes[0])?;
        let old = reserve_at(&db, &scopes, due)?;
        let denial = match case {
            "execution" => {
                attempt.source.generation += 1;
                ProviderRecoveryDispatchDenial::StaleExecution(
                    ProviderDispatchRejection::SnapshotSourceMismatch,
                )
            }
            "episode" => {
                current.revision += 1;
                ProviderRecoveryDispatchDenial::StaleEpisode
            }
            "budget" => {
                for index in 0..MAX_REQUESTS {
                    assert!(matches!(
                        db.charge_provider_recovery_dispatch_for_attempt(
                            &current,
                            &old,
                            &format!("charged-{index}"),
                            1,
                            &attempt,
                            due,
                        )?,
                        ProviderRecoveryDispatchResult::Charged(_),
                    ));
                    current = refresh(&db, &current)?;
                }
                ProviderRecoveryDispatchDenial::BudgetExhausted
            }
            "paused" => {
                db.transaction(|tx| {
                    tx.execute(
                        "UPDATE provider_recovery_episodes SET state = 'cooling', not_before = ?1",
                        [timestamp(due + Duration::minutes(1))],
                    )?;
                    Ok(())
                })?;
                ProviderRecoveryDispatchDenial::EpisodePaused
            }
            "duplicate" | "attempted" => {
                let ProviderRecoveryDispatchResult::Charged(dispatch) = db
                    .charge_provider_recovery_dispatch_for_attempt(
                        &current,
                        &old,
                        "fresh-request",
                        1,
                        &attempt,
                        due,
                    )?
                else {
                    anyhow::bail!("expected original dispatch");
                };
                if case == "attempted" {
                    assert!(db.record_provider_recovery_request_outcome(
                        &dispatch,
                        ProviderRecoveryRequestOutcome::NetworkFailure(&scopes[0]),
                        false,
                        due,
                    )?);
                    assert!(db.record_provider_admission_failure(
                        &old,
                        &scopes[0],
                        ProviderCooldownPolicy::Transport,
                        due,
                        None,
                        "test/model",
                    )?);
                }
                current = refresh(&db, &current)?;
                if case == "duplicate" {
                    ProviderRecoveryDispatchDenial::DuplicateRequest
                } else {
                    ProviderRecoveryDispatchDenial::ScopeAlreadyAttempted
                }
            }
            _ => unreachable!(),
        };
        let before = snapshot(&db, &scopes)?;
        let persisted = refresh(&db, &current)?;
        let id = if case == "attempted" {
            "another-request"
        } else {
            "fresh-request"
        };
        let ProviderRecoveryDispatchAdmission::Denied(actual) =
            admit(&db, &current, &scopes, id, &attempt, due)?
        else {
            anyhow::bail!("expected {denial:?} for {case}");
        };
        assert_eq!(actual, denial, "{case}");
        assert_eq!(snapshot(&db, &scopes)?, before, "{case}");
        let after = refresh(&db, &current)?;
        assert_eq!(after.request_count, persisted.request_count, "{case}");
        assert_eq!(after.total_requests, persisted.total_requests, "{case}");
        assert_eq!(after.revision, persisted.revision, "{case}");
    }
    Ok(())
}
