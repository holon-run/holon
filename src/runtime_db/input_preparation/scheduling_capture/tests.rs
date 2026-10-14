use super::*;
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
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
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
