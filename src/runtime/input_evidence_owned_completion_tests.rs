use super::*;
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "original-completion".into(),
        identity_incarnation: 1,
    }
}
#[test]
fn completion_reservation_grows_without_new_nonce_and_releases_with_last_guard() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let before = ledger.report()?;
    let reservation = ledger.reserve_owned_completion(
        &scope(),
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 64,
            metadata_estimate_bytes: 16,
        },
    )?;
    assert!(reservation.matches_ledger(&ledger));
    assert!(!reservation.matches_ledger(&InputEvidenceBudgetLedger::new(Default::default())?));
    assert!(reservation.matches_scope(&scope()));
    assert!(!reservation.matches_scope(&InputEvidenceScope {
        agent_id: "other".into(),
        identity_incarnation: 1
    }));
    assert!(!reservation.matches_scope(&InputEvidenceScope {
        agent_id: scope().agent_id,
        identity_incarnation: 2
    }));
    let nonce = ledger.inner.state.lock().unwrap().next_nonce;
    reservation.observe_material(4096, 128);
    let retained = reservation.guard();
    assert_eq!(ledger.inner.state.lock().unwrap().next_nonce, nonce);
    assert_eq!(
        ledger.report()?.owned,
        OwnedBytesEstimate {
            payload_bytes: 4160,
            metadata_estimate_bytes: 144
        }
    );
    assert_eq!(ledger.report()?.pending, before.pending);
    assert_eq!(
        ledger.report()?.reclaimable_history,
        before.reclaimable_history
    );
    drop(reservation);
    assert_eq!(ledger.report()?.observed_bytes, 4304);
    drop(retained);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn completion_reservation_fails_before_work_if_original_nonce_cannot_be_registered() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    ledger.inner.state.lock().unwrap().next_nonce = u64::MAX;
    let result = ledger.reserve_owned_completion(
        &scope(),
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate::default(),
    );
    assert!(matches!(
        result,
        Err(InputEvidenceBudgetError::CounterOverflow)
    ));
    assert!(ledger.inner.state.lock().unwrap().views.is_empty());
    Ok(())
}
#[test]
fn completed_material_is_still_registered_if_ledger_becomes_poisoned() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let reservation = ledger.reserve_owned_completion(
        &scope(),
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate::default(),
    )?;
    let shared = ledger.inner.clone();
    assert!(std::thread::spawn(move || {
        let _lock = shared.state.lock().unwrap();
        panic!("original ledger fault");
    })
    .join()
    .is_err());
    reservation.observe_material(1024, 16);
    assert_eq!(
        ledger
            .inner
            .state
            .lock()
            .unwrap_err()
            .into_inner()
            .views
            .values()
            .next()
            .unwrap()
            .owned
            .payload_bytes,
        1024
    );
    assert!(matches!(
        ledger.report(),
        Err(InputEvidenceBudgetError::Poisoned)
    ));
    drop(reservation);
    assert!(ledger
        .inner
        .state
        .lock()
        .unwrap_err()
        .into_inner()
        .views
        .is_empty());
    Ok(())
}
