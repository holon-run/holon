use super::super::super::{
    evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
    input_mutation_receipts::{
        canonical_command_digest, insert_input_mutation_receipt_tx,
        with_fresh_input_mutation_resolution_and_retained_material, InputMutationFamily,
        InputMutationOutcome, InputMutationResolution, InputMutationScopeOutcome,
        OriginalInputMutationAttempt,
    },
    RuntimeDb,
};
use super::*;

#[test]
fn actual_live_input_selectors_and_retained_report_precede_the_first_fresh_scope_allocation(
) -> Result<()> {
    use crate::runtime::{
        input_queries::{source_roles_for_fixture, InputQuerySpec},
        mutation_owner::MutationOwnerRegistry,
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey},
    };
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let owners = MutationOwnerRegistry::default();
    let owner = owners.claim(&f.scope.agent_id, f.scope.identity_incarnation)?;
    let required = [InputEvidenceKey::new(
        InputEvidenceFamily::Transcript,
        "original-required-\0-selector",
    )];
    let (reservation, lease, roles) = source_roles_for_fixture(
        &owner,
        &f.scope,
        InputQuerySpec {
            required_keys: &required,
            birth_after: Some(0),
        },
        &ledger,
        (64 * 1024 * 1024, 65536),
    )?;
    drop(roles);
    let source = reservation.input_query_source(&f.scope, &ledger, (64 * 1024 * 1024, 65536))?;
    let remaining = source.remaining_limits(&f.scope, &ledger, (64 * 1024 * 1024, 65536))?;
    let query_cost = (64 * 1024 * 1024 - remaining.0, 65536 - remaining.1);
    let report_prior = (1024 * 1024 + 1024, 32);
    let report_guard = ledger.register_owned_bytes(
        &f.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 1024 * 1024,
            metadata_estimate_bytes: 1024,
        },
    )?;
    let report_body = vec![3u8; 1024 * 1024];
    let baseline = ledger.report()?.observed_bytes;
    let scope_only = OriginalInputRecoveryMaterial::new(
        &f.scope,
        &ledger,
        (64 * 1024 * 1024, 65536),
        (0, 0),
        None,
    )?
    .input
    .usage();
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let combined = (
        report_prior.0 + query_cost.0 + scope_only.0,
        report_prior.1 + query_cost.1 + scope_only.1,
    );
    // A too-small complete phase must fail before the first receipt SQL too.
    f.db.connection()?
        .execute_batch("ALTER TABLE input_mutation_receipts RENAME TO forbidden_fresh_receipt")?;
    for limits in [(combined.0 - 1, combined.1), (combined.0, combined.1 - 1)] {
        assert!(report_prior.0 < limits.0 && query_cost.0 < limits.0);
        let error =
            f.db.transaction(|tx| {
                let mut phase = OriginalInputRecoveryMaterial::new_with_input_source(
                    &f.scope,
                    &ledger,
                    limits,
                    report_prior,
                    None,
                    Some(&source),
                )?;
                phase.receipt_tx(tx, f.attempt.prepared())
            })
            .err()
            .context("Input selectors were admitted after the fresh scope or receipt allocation")?;
        resource(&error);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let mut phase = OriginalInputRecoveryMaterial::new_with_input_source(
        &f.scope,
        &ledger,
        combined,
        report_prior,
        None,
        Some(&source),
    )?;
    assert_eq!(phase.input.usage(), combined);
    phase
        .input
        .observe_query_prior(source.roles(), source.owner_epoch())?;
    assert_eq!(
        phase.input.usage(),
        combined,
        "same actual selector guards must not debit twice"
    );
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop((report_body, report_guard, source, lease, reservation));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn fresh_input_query_loans_reject_foreign_binding_and_keep_original_receipt_semantics() -> Result<()>
{
    use crate::runtime::{
        input_queries::{source_roles_for_fixture, InputQuerySpec},
        mutation_owner::MutationOwnerRegistry,
    };
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let owners = MutationOwnerRegistry::default();
    let owner = owners.claim(&f.scope.agent_id, f.scope.identity_incarnation)?;
    let (reservation, lease, roles) = source_roles_for_fixture(
        &owner,
        &f.scope,
        InputQuerySpec {
            required_keys: &[],
            birth_after: Some(0),
        },
        &ledger,
        (64 * 1024 * 1024, 65536),
    )?;
    drop(roles);
    let source = reservation.input_query_source(&f.scope, &ledger, (64 * 1024 * 1024, 65536))?;
    let baseline = ledger.report()?.observed_bytes;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(OriginalInputRecoveryMaterial::new_with_input_source(
        &f.scope,
        &foreign,
        (64 * 1024 * 1024, 65536),
        (0, 0),
        None,
        Some(&source)
    )
    .is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    let successor = InputEvidenceScope {
        identity_incarnation: f.scope.identity_incarnation + 1,
        ..f.scope.clone()
    };
    assert!(OriginalInputRecoveryMaterial::new_with_input_source(
        &successor,
        &ledger,
        (64 * 1024 * 1024, 65536),
        (0, 0),
        None,
        Some(&source)
    )
    .is_err());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let ((), phase) = with_fresh_input_mutation_resolution_and_retained_material(
        &mut f.db.connection()?,
        &f.attempt,
        |tx| {
            let mut phase = OriginalInputRecoveryMaterial::new_with_input_source(
                &f.scope,
                &ledger,
                (64 * 1024 * 1024, 65536),
                (0, 0),
                None,
                Some(&source),
            )?;
            phase.receipt_tx(tx, f.attempt.prepared())?;
            Ok(phase)
        },
        |tx, resolution, phase| {
            assert!(
                matches!(resolution,InputMutationResolution::Present(ref r) if r.prepared == *f.attempt.prepared() && !r.outcome.applied)
            );
            let facts = phase.scope_facts_tx(tx, &f.scope, &ledger)?;
            assert_eq!(facts.identity.as_ref(), Some(&f.identity));
            assert_eq!(
                facts.state.as_ref().map(|state| state.id.as_str()),
                Some(f.scope.agent_id.as_str())
            );
            let before = phase.input.usage();
            phase
                .input
                .observe_query_prior(source.roles(), source.owner_epoch())?;
            assert_eq!(phase.input.usage(), before);
            Ok(())
        },
    )?;
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop((source, lease, reservation));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
use crate::runtime::input_evidence_budget::{InputMemoryCategory, OwnedBytesEstimate};
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
};

struct Fixture {
    _home: tempfile::TempDir,
    db: RuntimeDb,
    scope: InputEvidenceScope,
    identity: AgentIdentityRecord,
    attempt: OriginalInputMutationAttempt,
}
impl Fixture {
    fn new() -> Result<Self> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "original-report-fresh-phase".into(),
            identity_incarnation: 1,
        };
        let identity = AgentIdentityRecord::new(
            scope.agent_id.clone(),
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        let prepared = PreparedInputMutation::new(
            InputMutationFamily::WorkItem,
            "original-fresh-receipt".into(),
            vec![scope.clone()],
            canonical_command_digest(&serde_json::json!({"work":"exact-original"}))?,
            chrono::Utc::now(),
        )?;
        db.transaction(|tx| {
            upsert_agent_identity_tx(tx, &identity)?;
            upsert_agent_state_tx(tx, &AgentState::new(scope.agent_id.clone()))?;
            insert_input_mutation_receipt_tx(
                tx,
                &prepared,
                &InputMutationOutcome {
                    result: Default::default(),
                    applied: false,
                    delivery_receipt: None,
                    scopes: vec![InputMutationScopeOutcome {
                        install_keys: Default::default(),
                        scope: scope.clone(),
                        audit_events: Vec::new(),
                        notify_memory_index: false,
                        notify_scheduler: false,
                    }],
                },
                prepared.prepared_at(),
            )
            .map(|_| ())
        })?;
        let attempt = OriginalInputMutationAttempt::new(prepared);
        attempt.finish_original_attempt();
        Ok(Self {
            _home: home,
            db,
            scope,
            identity,
            attempt,
        })
    }
}
fn resource(error: &anyhow::Error) {
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
}

#[test]
fn retained_source_and_actual_receipt_share_the_first_phase_and_guards_survive_handoff(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let prior = (1024 * 1024 + 1024, 32);
    let source_guard = ledger.register_owned_bytes(
        &f.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 1024 * 1024,
            metadata_estimate_bytes: 1024,
        },
    )?;
    let source_body = vec![7u8; 1024 * 1024];
    assert_eq!(source_body.len(), 1024 * 1024);
    let baseline = ledger.report()?.observed_bytes;
    let incoming = f.db.transaction(|tx| {
        let mut phase = OriginalInputRecoveryMaterial::new(
            &f.scope,
            &ledger,
            (64 * 1024 * 1024, 65536),
            (0, 0),
            None,
        )?;
        phase.receipt_tx(tx, f.attempt.prepared())?;
        Ok(phase.input.usage())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let total = (prior.0 + incoming.0, prior.1 + incoming.1);
    assert!(incoming.0 < total.0 - 1 && prior.0 < total.0 - 1);
    for limits in [(total.0 - 1, total.1), (total.0, total.1 - 1)] {
        let callback = std::cell::Cell::new(false);
        let error = with_fresh_input_mutation_resolution_and_retained_material(
            &mut f.db.connection()?,
            &f.attempt,
            |tx| {
                let mut phase =
                    OriginalInputRecoveryMaterial::new(&f.scope, &ledger, limits, prior, None)?;
                phase.receipt_tx(tx, f.attempt.prepared())?;
                Ok(phase)
            },
            |_, _, _| {
                callback.set(true);
                Ok(())
            },
        )
        .err()
        .context("retained Source received a second full receipt allowance")?;
        resource(&error);
        assert!(!callback.get());
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let (receipt, phase) = with_fresh_input_mutation_resolution_and_retained_material(
        &mut f.db.connection()?,
        &f.attempt,
        |tx| {
            let mut phase =
                OriginalInputRecoveryMaterial::new(&f.scope, &ledger, total, prior, None)?;
            phase.receipt_tx(tx, f.attempt.prepared())?;
            Ok(phase)
        },
        |_, result, phase| {
            assert_eq!(phase.input.usage(), total);
            assert!(
                matches!(&result, InputMutationResolution::Present(r) if r.prepared == *f.attempt.prepared() && !r.outcome.applied)
            );
            Ok(result)
        },
    )?;
    assert!(
        ledger.report()?.observed_bytes > baseline,
        "fresh guards ended before owner installation"
    );
    drop(receipt);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop(source_body);
    drop(source_guard);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn all_actual_receipt_columns_are_raw_guarded_before_any_json_or_owned_decoder() -> Result<()> {
    for column in [
        "family",
        "command_digest",
        "scope_digest",
        "scopes_json",
        "prepared_at",
        "prepared_json",
        "outcome_json",
        "committed_at",
        "result_digest",
    ] {
        let f = Fixture::new()?;
        let malformed = "[".to_owned() + &"x".repeat(1024 * 1024);
        let connection = f.db.connection()?;
        // Corrupt only this temporary fixture, including normalized CHECK fields.
        connection.execute_batch("PRAGMA ignore_check_constraints=ON")?;
        connection.execute(
            &format!("UPDATE input_mutation_receipts SET {column}=?1 WHERE command_nonce=?2"),
            rusqlite::params![malformed, f.attempt.prepared().command_nonce()],
        )?;
        drop(connection);
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let callback = std::cell::Cell::new(false);
        let error = with_fresh_input_mutation_resolution_and_retained_material(
            &mut f.db.connection()?,
            &f.attempt,
            |tx| {
                let mut phase = OriginalInputRecoveryMaterial::new(
                    &f.scope,
                    &ledger,
                    (256 * 1024, 65536),
                    (0, 0),
                    None,
                )?;
                phase.receipt_tx(tx, f.attempt.prepared())?;
                Ok(phase)
            },
            |_, _, _| {
                callback.set(true);
                Ok(())
            },
        )
        .err()
        .context("oversized malformed receipt reached original decoder")?;
        resource(&error);
        assert!(!callback.get(), "{column}");
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn actual_receipt_native_object_keys_share_the_raw_phase_before_serde() -> Result<()> {
    let f = Fixture::new()?;
    let value: serde_json::Map<String, serde_json::Value> = (0..1000)
        .map(|i| (format!("key-{i}"), serde_json::Value::Null))
        .collect();
    let json = serde_json::to_string(&value)?;
    assert!(json.len() * 8 < 1024 * 1024);
    f.db.connection()?.execute(
        "UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",
        rusqlite::params![json, f.attempt.prepared().command_nonce()],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let error =
        f.db.transaction(|tx| {
            let mut phase = OriginalInputRecoveryMaterial::new(
                &f.scope,
                &ledger,
                (64 * 1024 * 1024, 512),
                (0, 0),
                None,
            )?;
            phase.receipt_tx(tx, f.attempt.prepared())
        })
        .err()
        .context("receipt object keys bypassed the original phase")?;
    resource(&error);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn retired_original_report_reads_identity_without_touching_successor_state() -> Result<()> {
    let mut f = Fixture::new()?;
    f.identity.incarnation = 2;
    f.db.transaction(|tx| upsert_agent_identity_tx(tx, &f.identity))?;
    f.db.connection()?
        .execute_batch("ALTER TABLE agent_states RENAME TO forbidden_successor_state")?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    f.db.transaction(|tx| {
        let mut phase = OriginalInputRecoveryMaterial::new(
            &f.scope,
            &ledger,
            (64 * 1024 * 1024, 65536),
            (0, 0),
            None,
        )?;
        let facts = phase.scope_facts_tx(tx, &f.scope, &ledger)?;
        assert_eq!(facts.identity.as_ref(), Some(&f.identity));
        assert!(facts.state.is_none() && facts.control_revision.is_none());
        let other = InputEvidenceBudgetLedger::new(Default::default())?;
        assert!(phase.scope_facts_tx(tx, &f.scope, &other).is_err());
        assert_eq!(other.report()?.observed_bytes, 0);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
