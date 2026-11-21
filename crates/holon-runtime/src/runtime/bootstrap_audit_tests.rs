use super::*;
use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        mutation_owner::{MutationOwner, MutationReservation},
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::context_inputs::ContextScopeFacts,
    types::{AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use sha2::{Digest, Sha256};

struct Fixture {
    _home: tempfile::TempDir,
    setup: RuntimeStorageSetup,
    scope: InputEvidenceScope,
    facts: ContextScopeFacts,
    budget: InputEvidenceBudgetLedger,
}

impl Fixture {
    fn new(with_state: bool) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let setup = prepare_runtime_storage_setup(
            "bootstrap-audit",
            home.path().join("agent"),
            home.path().to_path_buf(),
            None,
        )?;
        setup.runtime_db.initialize_sqlite_storage_domains()?;
        setup
            .runtime_db
            .agent_identities()
            .create_legacy_materialized_with_relations(
                &AgentIdentityRecord::new(
                    "bootstrap-audit",
                    AgentKind::Default,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
                &Default::default(),
            )?;
        if with_state {
            setup.runtime_db.transaction(|tx| {
                crate::runtime_db::evidence::upsert_agent_state_tx(
                    tx,
                    &AgentState::new("bootstrap-audit"),
                )
            })?;
        }
        let scope = InputEvidenceScope {
            agent_id: "bootstrap-audit".into(),
            identity_incarnation: 1,
        };
        let facts = setup.runtime_db.bootstrap_scope_facts(&scope)?;
        Ok(Self {
            _home: home,
            setup,
            scope,
            facts,
            budget: InputEvidenceBudgetLedger::new(Default::default())?,
        })
    }

    fn append(
        &self,
        reservation: Option<&mut MutationReservation>,
        event: AuditEvent,
    ) -> Result<()> {
        let prior = Arc::new(super::super::mutation_owner::IdentityMutationFacts {
            identity: self.facts.identity.clone().map(Arc::new),
            state: self.facts.state.clone(),
        });
        append_bootstrap_audit_event(
            &self.setup.storage,
            &self.setup.runtime_db,
            &self.scope,
            &prior,
            &self.budget,
            reservation,
            event,
        )
    }
}

#[test]
fn unloaded_audit_closes_metadata_phase_and_releases_original_owner() -> Result<()> {
    for with_state in [false, true] {
        let f = Fixture::new(with_state)?;
        let owner = MutationOwner::standalone(&f.scope.agent_id, f.scope.identity_incarnation);
        let mut reservation = owner.try_reserve()?;
        let prior = Arc::new(super::super::mutation_owner::IdentityMutationFacts {
            identity: f.facts.identity.clone().map(Arc::new),
            state: f.facts.state.clone(),
        });
        for _ in 0..2 {
            let event = AuditEvent::legacy("unloaded_original_audit", serde_json::json!({}));
            append_unloaded_audit_event(
                &f.setup.storage,
                &f.setup.runtime_db,
                &f.scope,
                &prior,
                &f.budget,
                &mut reservation,
                event.clone(),
            )?;
            assert!(reservation.recovery()?.is_none());
            f.setup.runtime_db.transaction(|tx| {
                let qualification: String = tx.query_row(
                    "SELECT qualification_state FROM current_read_qualification
                     WHERE agent_id=?1 AND identity_incarnation=?2 AND family='event' AND record_id=?3",
                    rusqlite::params![f.scope.agent_id, f.scope.identity_incarnation, event.id],
                    |row| row.get(0),
                )?;
                assert_eq!(qualification, "qualified");
                Ok(())
            })?;
        }
        assert_eq!(
            f.setup.runtime_db.bootstrap_scope_facts(&f.scope)?.state,
            f.facts.state
        );
        drop(reservation);
        assert!(owner.try_reserve()?.recovery()?.is_none());
    }
    Ok(())
}

#[test]
fn unloaded_audit_scope_and_rollback_leave_owner_reusable() -> Result<()> {
    let f = Fixture::new(true)?;
    let owner = MutationOwner::standalone(&f.scope.agent_id, f.scope.identity_incarnation);
    let mut reservation = owner.try_reserve()?;
    let prior = Arc::new(super::super::mutation_owner::IdentityMutationFacts {
        identity: f.facts.identity.clone().map(Arc::new),
        state: f.facts.state.clone(),
    });
    let foreign = InputEvidenceScope {
        identity_incarnation: f.scope.identity_incarnation + 1,
        ..f.scope.clone()
    };
    let event = AuditEvent::legacy("unloaded_rejected_audit", serde_json::json!({}));
    let error = append_unloaded_audit_event(
        &f.setup.storage,
        &f.setup.runtime_db,
        &foreign,
        &prior,
        &f.budget,
        &mut reservation,
        event.clone(),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("bootstrap Audit prior identity differs"));
    assert!(reservation.recovery()?.is_none());
    f.setup.runtime_db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_unloaded_audit BEFORE INSERT ON input_mutation_receipts
             WHEN NEW.family='audit' BEGIN SELECT RAISE(ABORT,'unloaded Audit rejected'); END;",
        )?;
        Ok(())
    })?;
    let error = append_unloaded_audit_event(
        &f.setup.storage,
        &f.setup.runtime_db,
        &f.scope,
        &prior,
        &f.budget,
        &mut reservation,
        event.clone(),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("unloaded Audit rejected"));
    assert!(reservation.recovery()?.is_none());
    assert!(!f
        .setup
        .runtime_db
        .audit_events()
        .has_event_by_id(&event.id)?);
    f.setup.runtime_db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_unloaded_audit")?;
        Ok(())
    })?;
    append_unloaded_audit_event(
        &f.setup.storage,
        &f.setup.runtime_db,
        &f.scope,
        &prior,
        &f.budget,
        &mut reservation,
        event,
    )?;
    assert!(reservation.recovery()?.is_none());
    assert_eq!(
        f.setup.runtime_db.bootstrap_scope_facts(&f.scope)?.state,
        f.facts.state
    );
    Ok(())
}

#[test]
fn bootstrap_audit_original_body_and_receipt_do_not_write_state() -> Result<()> {
    for with_state in [false, true] {
        let f = Fixture::new(with_state)?;
        f.setup.runtime_db.transaction(|tx| {
            tx.execute_batch(
                "CREATE TRIGGER audit_no_insert_state BEFORE INSERT ON agent_states
                 BEGIN SELECT RAISE(ABORT,'Audit inserted State'); END;
                 CREATE TRIGGER audit_no_update_state BEFORE UPDATE ON agent_states
                 BEGIN SELECT RAISE(ABORT,'Audit updated State'); END;",
            )?;
            Ok(())
        })?;
        let event = AuditEvent::legacy("bootstrap_original_audit", serde_json::json!({"n":1}));
        f.append(None, event.clone())?;
        let events = f.setup.storage.read_recent_events(8)?;
        let actual = events.iter().find(|actual| actual.id == event.id).unwrap();
        assert!(actual.event_seq > 0);
        let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(actual)?));
        f.setup.runtime_db.transaction(|tx| {
            let qualified: (String, String) = tx.query_row(
                "SELECT qualification_state,content_digest FROM current_read_qualification
                 WHERE agent_id=?1 AND identity_incarnation=?2 AND family='event' AND record_id=?3",
                rusqlite::params![f.scope.agent_id, f.scope.identity_incarnation, event.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(qualified.0, "qualified");
            assert_eq!(qualified.1, hash);
            let receipt: usize = tx.query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1 AND family='audit'",
                [&event.id], |row| row.get(0),
            )?;
            assert_eq!(receipt, 1);
            Ok(())
        })?;
        assert_eq!(
            f.setup.runtime_db.bootstrap_scope_facts(&f.scope)?.state,
            f.facts.state
        );
    }
    Ok(())
}

#[test]
fn bootstrap_audit_failure_keeps_prior_metadata_known_and_rolls_back_its_qualification(
) -> Result<()> {
    let f = Fixture::new(true)?;
    let owner = MutationOwner::standalone(&f.scope.agent_id, f.scope.identity_incarnation);
    let mut reservation = owner.try_reserve()?;
    reservation.begin("bootstrap-prior-phase".into())?;
    f.setup.runtime_db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_bootstrap_audit_receipt BEFORE INSERT ON input_mutation_receipts
             WHEN NEW.family='audit' BEGIN SELECT RAISE(ABORT,'bootstrap Audit receipt rejected'); END;",
        )?;
        Ok(())
    })?;
    let event = AuditEvent::legacy("bootstrap_audit_rejected", serde_json::json!({}));
    let error = f.append(Some(&mut reservation), event.clone()).unwrap_err();
    assert!(
        format!("{error:#}").contains("bootstrap Audit receipt rejected"),
        "{error:#}"
    );
    assert!(reservation.recovery()?.is_none());
    assert_eq!(
        owner.capture()?.unwrap().state.as_ref(),
        f.facts.state.as_deref().unwrap()
    );
    assert!(!f
        .setup
        .runtime_db
        .audit_events()
        .has_event_by_id(&event.id)?);
    f.setup.runtime_db.transaction(|tx| {
        let addresses: usize = tx.query_row(
            "SELECT COUNT(*) FROM current_read_qualification WHERE record_id=?1",
            [&event.id],
            |row| row.get(0),
        )?;
        assert_eq!(addresses, 0);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn bootstrap_audit_raw_address_is_not_adopted() -> Result<()> {
    let f = Fixture::new(true)?;
    let event = AuditEvent::legacy("raw_bootstrap_audit", serde_json::json!({}));
    f.setup.storage.append_event(&event)?;
    f.append(None, event.clone())?;
    f.setup.runtime_db.transaction(|tx| {
        let status: String = tx.query_row(
            "SELECT qualification_state FROM current_read_qualification
             WHERE agent_id=?1 AND identity_incarnation=?2 AND family='event' AND record_id=?3",
            rusqlite::params![f.scope.agent_id, f.scope.identity_incarnation, event.id],
            |row| row.get(0),
        )?;
        assert_eq!(status, "unprepared");
        Ok(())
    })?;
    Ok(())
}

#[test]
fn bootstrap_audit_rejects_foreign_prior_and_owner_before_settling_metadata() -> Result<()> {
    use crate::runtime::mutation_owner::IdentityMutationFacts;
    for with_state in [false, true] {
        for case in 0..9 {
            let f = Fixture::new(with_state)?;
            let mut prior = IdentityMutationFacts {
                identity: f.facts.identity.clone().map(Arc::new),
                state: f.facts.state.clone(),
            };
            let mut scope = f.scope.clone();
            let mut owner_agent = scope.agent_id.clone();
            let mut owner_incarnation = scope.identity_incarnation;
            let expected = match case {
                0 => {
                    prior.identity = None;
                    "prior identity missing"
                }
                1 => {
                    scope.agent_id = "foreign-bootstrap".into();
                    "prior identity differs"
                }
                2 => {
                    scope.identity_incarnation += 1;
                    "prior identity differs"
                }
                3 => {
                    Arc::make_mut(prior.identity.as_mut().unwrap()).agent_id =
                        "foreign-prior".into();
                    "prior identity differs"
                }
                4 => {
                    Arc::make_mut(prior.identity.as_mut().unwrap()).incarnation += 1;
                    "prior identity differs"
                }
                5 => {
                    Arc::make_mut(prior.identity.as_mut().unwrap()).status =
                        crate::types::AgentRegistryStatus::Deleting;
                    "prior identity differs"
                }
                6 => {
                    prior.state = Some(Arc::new(AgentState::new("foreign-state")));
                    "prior State differs"
                }
                7 => {
                    owner_agent = "foreign-owner".into();
                    "owner differs"
                }
                _ => {
                    owner_incarnation += 1;
                    "owner differs"
                }
            };
            let owner = MutationOwner::standalone(&owner_agent, owner_incarnation);
            let mut reservation = owner.try_reserve()?;
            let original_key = reservation.begin("original-bootstrap-phase".into())?;
            let original = reservation.recovery()?.unwrap();
            let event = AuditEvent::legacy("bootstrap_foreign_scope", serde_json::json!({}));
            let error = append_bootstrap_audit_event(
                &f.setup.storage,
                &f.setup.runtime_db,
                &scope,
                &Arc::new(prior),
                &f.budget,
                Some(&mut reservation),
                event.clone(),
            )
            .unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "case {case}: {error:#}"
            );
            let after = reservation.recovery()?.unwrap();
            assert_eq!(after.key, original_key);
            assert_eq!(after.phase, original.phase);
            assert!(after.known.is_none() && after.witness.is_none());
            assert!(!f
                .setup
                .runtime_db
                .audit_events()
                .has_event_by_id(&event.id)?);
            f.setup.runtime_db.transaction(|tx| {
                let (receipts, addresses): (usize, usize) = tx.query_row(
                    "SELECT (SELECT COUNT(*) FROM input_mutation_receipts),
                            (SELECT COUNT(*) FROM current_read_qualification)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!((receipts, addresses), (0, 0));
                Ok(())
            })?;
            assert_eq!(
                f.setup.runtime_db.bootstrap_scope_facts(&f.scope)?.state,
                f.facts.state
            );
        }
    }
    Ok(())
}
