use super::*;

#[test]
fn owned_registration_admits_bookkeeping_before_nonce_and_keeps_original_holder(
) -> anyhow::Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    for host in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let identity = scope();
        let header = (identity.agent_id.len() as u64 + 256) * 8;
        let registration = (if host { 0 } else { identity.agent_id.len() } as u64 + 256) * 8;
        for (bytes, records) in [
            (header + registration - 1, 128),
            (header + registration, 127),
        ] {
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &identity, bytes, records)?;
            let nonce = ledger.inner.state.lock().unwrap().next_nonce;
            let result = phase.read(|| {
                if host {
                    ledger.reserve_host_owned_completion(
                        InputMemoryCategory::Candidate,
                        OwnedBytesEstimate::default(),
                    )
                } else {
                    ledger.reserve_owned_completion(
                        &identity,
                        InputMemoryCategory::Candidate,
                        OwnedBytesEstimate::default(),
                    )
                }
                .map_err(Into::into)
            });
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            assert_eq!(ledger.inner.state.lock().unwrap().next_nonce, nonce);
            assert_eq!(ledger.report()?.observed_bytes, header);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &identity, header + registration, 128)?;
        let reservation = phase.read(|| {
            if host {
                ledger.reserve_host_owned_completion(
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate::default(),
                )
            } else {
                ledger.reserve_owned_completion(
                    &identity,
                    InputMemoryCategory::Candidate,
                    OwnedBytesEstimate::default(),
                )
            }
            .map_err(Into::into)
        })?;
        let nonce = ledger.inner.state.lock().unwrap().next_nonce;
        assert!(phase
            .read(|| {
                crate::diagnostics::resource_io::charge_owned_metadata(1, 1)?;
                Ok(())
            })
            .is_err());
        // Admitted completion never consults the exhausted phase or registers
        // another view. Its last holder retains the bookkeeping charge too.
        reservation.observe_material(32, 8);
        let last = reservation.guard();
        drop(reservation);
        drop(phase);
        assert_eq!(ledger.inner.state.lock().unwrap().next_nonce, nonce);
        assert_eq!(ledger.report()?.observed_bytes, header + registration + 40);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "original-completion".into(),
        identity_incarnation: 1,
    }
}
#[test]
fn borrowed_identity_reservation_preserves_scope_validation_nonce_and_last_guard() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let identity = scope();
    let initial = OwnedBytesEstimate {
        payload_bytes: 64,
        metadata_estimate_bytes: 16,
    };
    for (agent_id, incarnation) in [("", 1), (identity.agent_id.as_str(), 0)] {
        for borrowed in [false, true] {
            let result = if borrowed {
                ledger.reserve_owned_identity_completion(
                    agent_id,
                    incarnation,
                    InputMemoryCategory::Candidate,
                    initial,
                )
            } else {
                ledger.reserve_owned_completion(
                    &InputEvidenceScope {
                        agent_id: agent_id.into(),
                        identity_incarnation: incarnation,
                    },
                    InputMemoryCategory::Candidate,
                    initial,
                )
            };
            assert!(matches!(
                result,
                Err(InputEvidenceBudgetError::InvalidInput(
                    "scope needs agent ID and nonzero incarnation"
                ))
            ));
            assert!(ledger.report()?.scopes.is_empty());
        }
    }
    let reservation = ledger.reserve_owned_identity_completion(
        &identity.agent_id,
        identity.identity_incarnation,
        InputMemoryCategory::Candidate,
        initial,
    )?;
    assert!(reservation.matches_ledger(&ledger));
    assert!(reservation.matches_scope(&identity));
    assert_eq!(ledger.report()?.owned, initial);
    assert_eq!(ledger.report()?.scopes.len(), 1);
    assert_eq!(ledger.report()?.scopes[0].scope, identity);
    let nonce = ledger.inner.state.lock().unwrap().next_nonce;
    reservation.observe_material(32, 8);
    let last = reservation.guard();
    let weak = Arc::downgrade(&last);
    assert_eq!(ledger.inner.state.lock().unwrap().next_nonce, nonce);
    drop(reservation);
    assert_eq!(ledger.report()?.observed_bytes, 120);
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    ledger.inner.state.lock().unwrap().next_nonce = u64::MAX;
    assert!(matches!(
        ledger.reserve_owned_identity_completion(
            &identity.agent_id,
            identity.identity_incarnation,
            InputMemoryCategory::Candidate,
            initial,
        ),
        Err(InputEvidenceBudgetError::CounterOverflow)
    ));
    assert!(ledger.inner.state.lock().unwrap().views.is_empty());
    Ok(())
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
