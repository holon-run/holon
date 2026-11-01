use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::{
    config::StateMemoryLimits,
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        scheduling_snapshot::{
            load_scheduling_domain_snapshot_tx, prepare_scheduling_domain_snapshot_tx,
        },
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility,
    },
};
use rusqlite::Connection;
use std::collections::BTreeSet;

fn fixture() -> Result<(
    tempfile::TempDir,
    RuntimeDb,
    InputEvidenceScope,
    AgentState,
    InputEvidenceBudgetLedger,
)> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("runtime.sqlite");
    let scope = InputEvidenceScope {
        agent_id: "input-scheduling-selector".into(),
        identity_incarnation: 1,
    };
    let state = AgentState::new(&scope.agent_id);
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut connection, migration)?;
    }
    let tx = connection.transaction()?;
    let identity = AgentIdentityRecord::new(
        &scope.agent_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    upsert_agent_identity_tx(&tx, &identity)?;
    upsert_agent_state_tx(&tx, &state)?;
    tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db','2026-10-01T00:00:00.000Z')", [])?;
    tx.commit()?;
    drop(connection);
    let db = RuntimeDb::open_and_migrate(&path, home.path().join("runtime.lock"))?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let request = SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    };
    drop(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger))?,
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok((home, db, scope, state, ledger))
}
fn input_request(
    records: usize,
    bytes: usize,
) -> crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
    crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        query: None,
        mandatory_roots: BTreeSet::new(),
        windows: vec![],
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: records,
        byte_limit: bytes,
    }
}
#[test]
fn input_scheduling_selector_rejects_before_clone_and_corrupt_saved_coverage_decode() -> Result<()>
{
    let (_home, db, scope, state, ledger) = fixture()?;
    // Deliberate saved metadata corruption: loader validation must not run first.
    db.connection()?.execute(
        "UPDATE scheduling_domain_current SET revision=revision+1 WHERE agent_id=?1",
        [&scope.agent_id],
    )?;
    let before: (u64, String) = db.connection()?.query_row(
        "SELECT revision,coverage_json FROM scheduling_domain_current WHERE agent_id=?1",
        [&scope.agent_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    for (records, bytes, keys) in [(16, 16 * 1024 * 1024, 3), (4096, 4096, 1)] {
        let capture = SchedulingCaptureRequest {
            include_live: false,
            history_limit: 0,
            required_keys: (0..keys)
                .map(|i| {
                    SchedulingKey::new(
                        crate::runtime::resident_scheduling::SchedulingFamily::Timer,
                        format!("{i}-{}", "x".repeat(8192)),
                    )
                })
                .collect(),
        };
        let request = input_request(records, bytes);
        let result = db.transaction(|tx| {
            crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
                tx, &scope, &state, &request, 12, &ledger, &capture,
            )
        });
        let error = result
            .err()
            .context("over-budget owned selector accepted")?;
        assert!(
            matches!(
                error.downcast_ref::<SchedulingSnapshotError>(),
                Some(SchedulingSnapshotError::ResourceExhausted(
                    "Input-owned Scheduling selector"
                ))
            ),
            "{error:#}"
        );
        let after: (u64, String) = db.connection()?.query_row(
            "SELECT revision,coverage_json FROM scheduling_domain_current WHERE agent_id=?1",
            [&scope.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(before, after);
        let aliases: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_domain_records WHERE agent_id=?1",
            [&scope.agent_id],
            |r| r.get(0),
        )?;
        assert_eq!(aliases, 0);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn input_owned_selector_and_actual_nested_loader_share_remaining_record_allowance() -> Result<()> {
    let (_home, db, scope, _state, ledger) = fixture()?;
    let capture = SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::from([SchedulingKey::new(
            crate::runtime::resident_scheduling::SchedulingFamily::Timer,
            "absent-selected",
        )]),
    };
    let mut request = SchedulingDomainLoadRequest {
        capture: capture.clone(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    };
    drop(
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger))?,
    );
    // Find this real SQL view's smallest standalone read allowance. No candidate
    // owns material between trials; the final call then adds one live parent copy.
    let (mut low, mut high) = (1, 4096);
    while low < high {
        let middle = low + (high - low) / 2;
        request.record_limit = middle;
        match db.transaction(|tx| load_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger))
        {
            Ok(value) => {
                drop(value);
                high = middle;
            }
            Err(error) => {
                assert!(
                    matches!(
                        error.downcast_ref::<SchedulingSnapshotError>(),
                        Some(SchedulingSnapshotError::ResourceExhausted(_))
                    ),
                    "{error:#}"
                );
                low = middle + 1;
            }
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    request.record_limit = low;
    drop(db.transaction(|tx| load_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger))?);
    let owned =
        PreparedSchedulingRequest::new(&scope, Some(&capture), low, request.byte_limit, &ledger)?;
    assert!(
        ledger.report()?.observed_bytes > 0,
        "owned selector has a real Candidate guard"
    );
    let error = db
        .transaction(|tx| load_scheduling_domain_snapshot_tx(tx, &scope, &owned.request, &ledger))
        .err()
        .context("parent selector and loader each spent the full allowance")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingSnapshotError>(),
            Some(SchedulingSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    drop(owned);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn input_scheduling_selector_uses_context_remainder_before_saved_coverage_decode() -> Result<()> {
    let (_home, db, scope, state, ledger) = fixture()?;
    let capture = SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::from([SchedulingKey::new(
            crate::runtime::resident_scheduling::SchedulingFamily::Timer,
            "context-selected",
        )]),
    };
    let request = input_request(4096, 16 * 1024 * 1024);
    // Same selector and limits pass with the original phase's unspent remainder.
    let ample = ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
    {
        let _entry = ample.enter();
        drop(db.transaction(|tx| {
            crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
                tx, &scope, &state, &request, 12, &ledger, &capture,
            )
        })?);
    }
    ample.finish()?;
    drop(ample);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.connection()?.execute(
        "UPDATE scheduling_domain_current SET revision=revision+1 WHERE agent_id=?1",
        [&scope.agent_id],
    )?;
    let saved = || -> Result<(u64, String, usize)> {
        Ok(db.connection()?.query_row(
            "SELECT revision,coverage_json,(SELECT COUNT(*) FROM scheduling_domain_records WHERE agent_id=?1) FROM scheduling_domain_current WHERE agent_id=?1",
            [&scope.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    };
    let before = saved()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
    {
        let _entry = phase.enter();
        let remaining = 4096 - phase.finish()?.1;
        crate::diagnostics::resource_io::charge_owned_metadata(0, (remaining - 8) / 8)?;
        let error = db
            .transaction(|tx| {
                crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
                    tx, &scope, &state, &request, 12, &ledger, &capture,
                )
            })
            .err()
            .context("selector ignored the enclosing Context remainder")?;
        assert!(
            error
                .to_string()
                .contains("ResourceExhausted: context resource read material"),
            "{error:#}"
        );
        assert!(phase.finish().is_err());
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(before, saved()?);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn input_scheduling_selector_accumulates_context_credit_and_keeps_last_phase_guard() -> Result<()> {
    let (_home, _db, scope, _state, ledger) = fixture()?;
    let capture = SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::from([SchedulingKey::new(
            crate::runtime::resident_scheduling::SchedulingFamily::Timer,
            "context-holder",
        )]),
    };
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
    let header = phase.finish()?;
    let cost = {
        let _entry = phase.enter();
        let owned = PreparedSchedulingRequest::new(
            &scope,
            Some(&capture),
            4096,
            16 * 1024 * 1024,
            &ledger,
        )?;
        assert!(Arc::ptr_eq(
            owned._context_material.as_ref().unwrap(),
            &phase.guard()?
        ));
        let usage = phase.finish()?;
        (usage.0 - header.0, usage.1 - header.1)
    };
    assert!(cost.0 > 0 && cost.1 > 0);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    // Exactly one copy fits; a second copy cannot reset the phase allowance.
    let exact = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        16 * 1024 * 1024,
        header.1 + cost.1,
    )?;
    let owned = {
        let _entry = exact.enter();
        let owned = PreparedSchedulingRequest::new(
            &scope,
            Some(&capture),
            4096,
            16 * 1024 * 1024,
            &ledger,
        )?;
        assert_eq!(exact.finish()?, (header.0 + cost.0, header.1 + cost.1));
        assert!(PreparedSchedulingRequest::new(
            &scope,
            Some(&capture),
            4096,
            16 * 1024 * 1024,
            &ledger,
        )
        .is_err());
        assert!(exact.finish().is_err());
        owned
    };
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let last = owned._context_material.as_ref().unwrap().clone();
    drop(exact);
    assert!(ledger.report()?.observed_bytes > 0);
    assert_eq!(owned.request.capture.required_keys, capture.required_keys);
    drop(owned);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    // No active Context keeps the independent selector gate and lifetime.
    let independent =
        PreparedSchedulingRequest::new(&scope, Some(&capture), 4096, 16 * 1024 * 1024, &ledger)?;
    assert!(independent._context_material.is_none());
    drop(independent);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scheduling_loader_copies_share_context_remainder_and_rollback() -> Result<()> {
    use crate::diagnostics::resource_io::{admit_cloned_value, charge_owned_metadata};
    use crate::runtime::resident_scheduling::SchedulingFamily;
    use crate::runtime_db::scheduling_snapshot::material_cost_fixture::ExpectedSqlMaterial;
    let (_home, db, scope, _state, ledger) = fixture()?;
    let mut request = SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest {
            include_live: false,
            history_limit: 0,
            required_keys: BTreeSet::from([SchedulingKey::new(
                SchedulingFamily::Timer,
                "loader-prior",
            )]),
        },
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    };
    let prior = db
        .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger))?
        .require_current()?;
    request.capture.required_keys =
        BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, "loader-next")]);
    // Records depend on structure, not key text. The new absent selector has
    // the same coverage shape as the prior one, but would write a new alias.
    let sql = ExpectedSqlMaterial::new(&db, &scope);
    let measure = ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
    let (one_copy, two_copies, full) = {
        let _entry = measure.enter();
        sql.before_selection(&request, prior.resident.coverage())?;
        admit_cloned_value(&request.capture.required_keys)?;
        let one = measure.finish()?.1;
        admit_cloned_value(&request.capture.required_keys)?;
        let two = measure.finish()?.1;
        // No live/history/prior keys in this fixture: both traversal
        // worklists have the same shape as the requested absent key.
        for family in [
            SchedulingFamily::Timer,
            SchedulingFamily::TimerWake,
            SchedulingFamily::ExternalTrigger,
        ] {
            sql.ids(family, true, None, &BTreeSet::new())?;
            sql.ids(
                family,
                false,
                Some(request.capture.history_limit + 1),
                &BTreeSet::new(),
            )?;
        }
        admit_cloned_value(&request.capture.required_keys)?;
        admit_cloned_value(&request.capture.required_keys)?;
        let manifests: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_domain_captures WHERE seed_id=(SELECT seed_id FROM agent_episode_continuity WHERE agent_id=?1)",
            [&scope.agent_id], |row| row.get(0),
        )?;
        for _ in 0..manifests {
            sql.charge(128, 0)?;
        }
        // The final coverage moves into the resident; it is not cloned.
        // This fixture changes only an equal-length absent key, not its shape.
        sql.charge(serde_json::to_vec(prior.resident.coverage())?.len(), 1)?;
        // One absent observation still inserts one borrowed validation node.
        charge_owned_metadata(128, 1)?;
        // Validation and budget classification both borrow their root indexes.
        // The resident metadata view owns one scope registration as well.
        charge_owned_metadata((scope.agent_id.len() + 256) as u64, 8)?;
        (one, two, measure.finish()?.1)
    };
    assert!(full > two_copies && two_copies > one_copy);
    drop(measure);
    drop(prior);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let saved = || -> Result<(u64, String, usize)> {
        Ok(db.connection()?.query_row(
            "SELECT revision,coverage_json,(SELECT COUNT(*) FROM scheduling_domain_records WHERE agent_id=?1) FROM scheduling_domain_current WHERE agent_id=?1",
            [&scope.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    };
    let before = saved()?;
    // One selector fits but the second cannot; two fit but core cannot.
    for records in [one_copy, two_copies] {
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, records)?;
        {
            let _entry = phase.enter();
            let error = db
                .transaction(|tx| {
                    prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger)
                })
                .err()
                .context("loader reset the original phase allowance")?;
            assert!(
                error
                    .to_string()
                    .contains("ResourceExhausted: context resource read material"),
                "{error:#}"
            );
        }
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(saved()?, before);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let exact = ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, full)?;
    {
        let _entry = exact.enter();
        let prepared = db
            .transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger))
            .context("independently accounted loader allowance must fit")?
            .require_current()?;
        assert_eq!(
            prepared.resident.coverage().required_keys,
            request.capture.required_keys
        );
        assert_eq!(exact.finish()?.1, full);
    }
    drop(exact);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scheduling_loader_worklist_gates_precede_alias_writes() -> Result<()> {
    use crate::diagnostics::resource_io::admit_cloned_value;
    use crate::runtime::resident_scheduling::SchedulingFamily;
    use crate::runtime_db::scheduling_snapshot::material_cost_fixture::ExpectedSqlMaterial;
    let (_home, db, scope, _state, ledger) = fixture()?;
    let request = SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest {
            include_live: false,
            history_limit: 0,
            required_keys: BTreeSet::from([SchedulingKey::new(
                SchedulingFamily::Timer,
                format!("worklist-{}", "x".repeat(8192)),
            )]),
        },
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    };
    let coverage = db.connection()?.query_row(
        "SELECT coverage_json FROM scheduling_domain_current WHERE agent_id=?1",
        [&scope.agent_id],
        |row| row.get::<_, String>(0),
    )?;
    let coverage = serde_json::from_str(&coverage)?;
    let sql = ExpectedSqlMaterial::new(&db, &scope);
    let measure = ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
    let mut prefix = Vec::new();
    {
        let _entry = measure.enter();
        // required_keys -> coverage, selected, core, pending. There are no
        // live/history/prior keys, so all four copies have identical shape.
        sql.before_selection(&request, &coverage)?;
        for index in 0..4 {
            if index == 2 {
                for family in [
                    SchedulingFamily::Timer,
                    SchedulingFamily::TimerWake,
                    SchedulingFamily::ExternalTrigger,
                ] {
                    sql.ids(family, true, None, &BTreeSet::new())?;
                    sql.ids(
                        family,
                        false,
                        Some(request.capture.history_limit + 1),
                        &BTreeSet::new(),
                    )?;
                }
            }
            admit_cloned_value(&request.capture.required_keys)?;
            prefix.push(measure.finish()?);
        }
        // The missing required key is checked against each fixed checkpoint.
        let manifests: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM scheduling_domain_captures WHERE seed_id=(SELECT seed_id FROM agent_episode_continuity WHERE agent_id=?1)",
            [&scope.agent_id], |row| row.get(0),
        )?;
        for _ in 0..manifests {
            sql.charge(128, 0)?;
        }
        prefix[3] = measure.finish()?;
    }
    drop(measure);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.connection()?.execute_batch(
        "CREATE TRIGGER worklist_alias_sentinel BEFORE INSERT ON scheduling_domain_records
         BEGIN SELECT RAISE(ABORT, 'worklist alias sentinel reached'); END;",
    )?;
    let before: (u64, String) = db.connection()?.query_row(
        "SELECT revision,coverage_json FROM scheduling_domain_current WHERE agent_id=?1",
        [&scope.agent_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    // Exhaust bytes and records independently at each worklist boundary.
    // Sufficient credit must reach the sentinel, not fail in qualification.
    for (copies, exhausted) in [(2, true), (3, true), (4, false)] {
        for byte_bound in [false, true] {
            let (bytes, records) = prefix[copies - 1];
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                if byte_bound { bytes } else { 16 * 1024 * 1024 },
                if byte_bound { 4096 } else { records },
            )?;
            {
                let _entry = phase.enter();
                let error = db
                    .transaction(|tx| {
                        prepare_scheduling_domain_snapshot_tx(tx, &scope, &request, &ledger)
                    })
                    .err()
                    .context("alias sentinel did not stop loader")?;
                let text = format!("{error:#}");
                if exhausted {
                    assert!(
                        text.contains("ResourceExhausted: context resource read material"),
                        "{text}"
                    );
                    assert!(!text.contains("worklist alias sentinel reached"), "{text}");
                } else {
                    assert!(text.contains("worklist alias sentinel reached"), "{text}");
                }
            }
            if exhausted {
                assert!(phase.finish().is_err());
            } else {
                assert_eq!(phase.finish()?, prefix[3]);
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let after: (u64, String) = db.connection()?.query_row(
                "SELECT revision,coverage_json FROM scheduling_domain_current WHERE agent_id=?1",
                [&scope.agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(after, before);
            let aliases: usize = db.connection()?.query_row(
                "SELECT COUNT(*) FROM scheduling_domain_records WHERE agent_id=?1",
                [&scope.agent_id],
                |row| row.get(0),
            )?;
            assert_eq!(aliases, 0);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn scheduling_loader_context_guard_survives_input_install_replacement_and_last_capture(
) -> Result<()> {
    use crate::runtime::resident_scheduling::{CommittedSchedulingDelta, SchedulingFamily};
    let (_home, db, scope, state, ledger) = fixture()?;
    let capture = SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::from([SchedulingKey::new(
            SchedulingFamily::Timer,
            "loader-holder",
        )]),
    };
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 16 * 1024 * 1024, 4096)?;
    let weak = Arc::downgrade(&phase.guard()?);
    // Input preparation drops its temporary Scheduling snapshot and moves the
    // resident Arc through the same handoff used by the owner installation.
    let installed = {
        let _entry = phase.enter();
        db.transaction(|tx| {
            crate::runtime_db::input_preparation::prepare_input_with_scheduling_tx(
                tx,
                &scope,
                &state,
                &input_request(4096, 16 * 1024 * 1024),
                12,
                &ledger,
                &capture,
            )
        })?
        .scheduling
    };
    phase.finish()?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert!(weak.upgrade().is_some());
    let delta = CommittedSchedulingDelta {
        scope: scope.clone(),
        base_revision: installed.coverage().revision,
        revision: installed.coverage().revision,
        coverage: installed.coverage().clone(),
        records: installed.records_for_test().cloned().collect(),
    };
    let replacement = installed.with_committed(&delta)?;
    drop(installed);
    assert!(weak.upgrade().is_some());
    let first = replacement.capture(&capture)?;
    let second = Arc::new(replacement.capture(&capture)?);
    assert_eq!(second.coverage.required_keys, capture.required_keys);
    let last = second.clone();
    drop(replacement);
    drop(delta);
    drop(first);
    drop(second);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
