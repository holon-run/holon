use super::*;
use crate::{
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        repositories::upsert_turn_record_tx,
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility,
    },
};
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "guarded-input-record".into(),
        identity_incarnation: 1,
    }
}
fn policy() -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([Key::new(Family::Turn, "actual-selected-turn")]),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: MAX_RECORDS,
        byte_limit: MAX_METADATA_BYTES,
    }
}
fn fresh(nodes: usize) -> Result<(tempfile::TempDir, RuntimeDb, TurnRecord)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let mut turn = TurnRecord::new(&scope().agent_id, "actual-selected-turn", 1);
    turn.waiting_condition_ids = vec![String::new(); nodes];
    db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                &scope().agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new(&scope().agent_id))?;
        upsert_turn_record_tx(tx, &turn)?;
        let delta = register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Turn(turn.clone())],
        )?;
        assert_eq!(delta.records.len(), 1);
        assert!(delta.records[0].locator.revision > 0);
        Ok(())
    })?;
    Ok((home, db, turn))
}
fn observe(
    db: &RuntimeDb,
    request: &InputEvidenceLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(InputEvidenceBootstrapSnapshot, Vec<Arc<BudgetViewGuard>>)> {
    db.transaction(|tx| {
        let scope = scope();
        let mut material = InputReadMaterial::new(&scope, request, ledger, (0, 0))?;
        let snapshot = prepare_input_evidence_snapshot_with_material_tx(
            tx,
            &scope,
            request,
            false,
            &mut material,
        )?;
        Ok((snapshot, material.into_guards()))
    })
}
#[test]
fn actual_selected_input_raw_metadata_body_and_affinity_reject_before_owned_decode() -> Result<()> {
    for mode in 0..3 {
        let (_home, db, _turn) = fresh(0)?;
        let large = "[".to_owned() + &"x".repeat(256 * 1024);
        db.connection()?.execute(
            match mode {
                0 => "UPDATE turn_records SET turn_index=?1 WHERE turn_id=?2",
                1 => "UPDATE input_evidence_current SET fingerprint_json=?1 WHERE record_id=?2",
                _ => "UPDATE turn_records SET payload_json=?1 WHERE turn_id=?2",
            },
            params![large, "actual-selected-turn"],
        )?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let mut request = policy();
        request.byte_limit = 128 * 1024;
        let before:(i64,usize)=db.connection()?.query_row("SELECT revision,length(CAST(fingerprint_json AS BLOB)) FROM input_evidence_current WHERE record_id='actual-selected-turn'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let error = observe(&db, &request, &ledger)
            .err()
            .context("selected raw source must reject before malformed/type decoder")?;
        assert!(
            matches!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(db.connection()?.query_row("SELECT revision,length(CAST(fingerprint_json AS BLOB)) FROM input_evidence_current WHERE record_id='actual-selected-turn'",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,usize>(1)?)))?,before);
    }
    Ok(())
}
#[test]
fn actual_selected_input_raw_fit_native_body_and_guard_handoff() -> Result<()> {
    let (_home, db, turn) = fresh(2048)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut small = policy();
    small.record_limit = 3000;
    let raw: usize = db.connection()?.query_row(
        "SELECT length(CAST(payload_json AS BLOB)) FROM turn_records WHERE turn_id=?1",
        [&turn.turn_id],
        |r| r.get(0),
    )?;
    assert!(raw * 8 < small.byte_limit);
    let error = observe(&db, &small, &ledger)
        .err()
        .context("actual typed body native slots exceed small record cap")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (snapshot, guards) = observe(&db, &policy(), &ledger)?;
    let InputEvidenceBootstrapSnapshot::Current(snapshot) = snapshot else {
        anyhow::bail!("actual current source retired")
    };
    let captured = snapshot.resident.capture()?;
    let actual = captured
        .lookup(&Key::new(Family::Turn, &turn.turn_id))?
        .context("actual selected Turn missing")?;
    assert_eq!(actual.record, PreparedInputEvidence::Turn(turn));
    assert!(ledger.report()?.observed_bytes > 0);
    let retained = guards.clone();
    drop(guards);
    drop(snapshot);
    drop(captured);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(retained);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn actual_selected_input_foreign_malformed_body_is_never_decoded_or_adopted() -> Result<()> {
    let (_home, db, _turn) = fresh(0)?;
    let foreign = TurnRecord::new("foreign-input-owner", "foreign-selected-turn", 1);
    db.transaction(|tx| upsert_turn_record_tx(tx, &foreign))?;
    db.connection()?.execute(
        "UPDATE turn_records SET payload_json=?1 WHERE turn_id=?2",
        params!["[".to_owned() + &"bad".repeat(1024 * 1024), foreign.turn_id],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut request = policy();
    request.mandatory_roots = BTreeSet::from([Key::new(Family::Turn, &foreign.turn_id)]);
    let error = observe(&db, &request, &ledger)
        .err()
        .context("foreign source must remain unqualified")?;
    assert!(
        matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::NeedsPreparation(id)) if id.contains(&foreign.turn_id)),
        "{error:#}"
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM input_evidence_current WHERE record_id=?1",
            [foreign.turn_id],
            |r| r.get::<_, usize>(0)
        )?,
        0
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
