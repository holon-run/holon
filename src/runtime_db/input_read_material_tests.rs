use super::*;
use crate::{
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        RuntimeDb,
    },
    types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentState, AgentVisibility},
};
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "input-material".into(),
        identity_incarnation: 1,
    }
}
fn request() -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: MAX_RECORDS,
        byte_limit: MAX_METADATA_BYTES,
    }
}
fn fresh() -> Result<(
    tempfile::TempDir,
    RuntimeDb,
    AgentIdentityRecord,
    AgentState,
)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let identity = AgentIdentityRecord::new(
        scope().agent_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    let state = AgentState::new(scope().agent_id);
    db.transaction(|tx| {
        upsert_agent_identity_tx(tx, &identity)?;
        upsert_agent_state_tx(tx, &state)
    })?;
    Ok((home, db, identity, state))
}
fn resource(error: anyhow::Error) {
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
}
#[test]
fn actual_input_scope_raw_guard_precedes_identity_and_state_json_decoders() -> Result<()> {
    for state in [false, true] {
        let (_home, db, _identity, _state) = fresh()?;
        let large = "[".to_owned() + &"x".repeat(1024 * 1024);
        db.connection()?.execute(
            if state {
                "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2"
            } else {
                "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2"
            },
            params![large, scope().agent_id],
        )?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let mut policy = request();
        policy.byte_limit = 64 * 1024;
        let actual_scope = scope();
        for recovery in [false, true] {
            let error = db.transaction(|tx| {
                let scheduling = crate::runtime::resident_scheduling::SchedulingCaptureRequest::current();
                if recovery {
                    super::super::super::input_preparation::prepare_original_input_with_scheduling_tx(
                        tx,&actual_scope,&_state,&policy,0,&ledger,&scheduling,
                    ).map(|_|())
                } else {
                    super::super::super::input_preparation::prepare_input_with_scheduling_tx(
                        tx,&actual_scope,&_state,&policy,0,&ledger,&scheduling,
                    ).map(|_|())
                }
            }).err().context("actual joint actor must hit raw guard before malformed JSON")?;
            resource(error);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        let bytes: usize = db.connection()?.query_row(
            if state {
                "SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1"
            } else {
                "SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1"
            },
            [scope().agent_id],
            |r| r.get(0),
        )?;
        assert_eq!(bytes, 1024 * 1024 + 1);
    }
    Ok(())
}
#[test]
fn actual_input_scope_raw_fit_native_state_nodes_backpressure_before_decode() -> Result<()> {
    let (_home, db, identity, mut state) = fresh()?;
    state.attached_workspaces = vec![String::new(); 2048];
    db.transaction(|tx| upsert_agent_state_tx(tx, &state))?;
    let raw: usize = db.connection()?.query_row(
        "SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1",
        [scope().agent_id],
        |r| r.get(0),
    )?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let actual_scope = scope();
    let mut small = request();
    small.record_limit = 1024;
    assert!(raw * 8 < small.byte_limit);
    let error = db
        .transaction(|tx| {
            let mut material = InputReadMaterial::new(&actual_scope, &small, &ledger, (0, 0))?;
            material.scope_facts_tx(tx).map(|_| ())
        })
        .err()
        .context("native count must fail before typed State allocation")?;
    resource(error);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (facts, guards) = db.transaction(|tx| {
        let mut material = InputReadMaterial::new(&actual_scope, &request(), &ledger, (0, 0))?;
        let facts = material.scope_facts_tx(tx)?;
        Ok((facts, material.into_guards()))
    })?;
    assert_eq!(facts.identity, Some(identity));
    assert_eq!(facts.state, Some(state));
    assert!(ledger.report()?.observed_bytes > 0);
    let cloned_guards = guards.clone();
    drop(guards);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(facts);
    drop(cloned_guards);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn actual_input_selectors_debit_live_prior_once_before_first_copy() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let actual_scope = scope();
    let mut policy = request();
    policy.byte_limit = 64 * 1024;
    policy
        .mandatory_roots
        .insert(Key::new(Family::Message, "x".repeat(2048)));
    let prior = ledger.register_owned_bytes(
        &actual_scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 56 * 1024,
            metadata_estimate_bytes: 0,
        },
    )?;
    let baseline = ledger.report()?.observed_bytes;
    let mut material = InputReadMaterial::new(&actual_scope, &policy, &ledger, (56 * 1024, 0))?;
    resource(
        material
            .selectors(&policy)
            .err()
            .context("prior plus selector exceeds original cap")?,
    );
    drop(material);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop(prior);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut material = InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
    material.selectors(&policy)?;
    let source_bytes = ledger.report()?.observed_bytes;
    material.observe_projected(4096, 8)?;
    assert!(ledger.report()?.observed_bytes > source_bytes);
    drop(material);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    // Clamp original limits before subtracting a previously observed prior.
    let too_large = InputReadMaterial::new(
        &actual_scope,
        &request(),
        &ledger,
        (MAX_METADATA_BYTES + 1, 0),
    );
    resource(
        too_large
            .err()
            .context("hard cap must reject large prior")?,
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn actual_input_scope_never_traverses_foreign_json_or_creates_missing_state() -> Result<()> {
    let (_home, db, mut foreign, state) = fresh()?;
    foreign.agent_id = "foreign-input-material".into();
    db.transaction(|tx| upsert_agent_identity_tx(tx, &foreign))?;
    db.connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        params![
            "[".to_owned() + &"bad".repeat(1024 * 1024),
            foreign.agent_id
        ],
    )?;
    db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [scope().agent_id],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let actual_scope = scope();
    let (facts, guards) = db.transaction(|tx| {
        let mut material = InputReadMaterial::new(&actual_scope, &request(), &ledger, (0, 0))?;
        let facts = material.scope_facts_tx(tx)?;
        Ok((facts, material.into_guards()))
    })?;
    assert_eq!(
        facts.identity.as_ref().map(|x| x.agent_id.as_str()),
        Some(state.id.as_str())
    );
    assert!(facts.state.is_none() && facts.control_revision.is_none());
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM agent_states", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    drop(facts);
    drop(guards);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn actual_input_old_incarnation_never_decodes_successor_state() -> Result<()> {
    let (_home, db, mut identity, _state) = fresh()?;
    identity.incarnation = 2;
    db.transaction(|tx| upsert_agent_identity_tx(tx, &identity))?;
    let malformed = "[".to_owned() + &"x".repeat(1024 * 1024);
    db.connection()?.execute(
        "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
        params![malformed, scope().agent_id],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let actual_scope = scope();
    let mut policy = request();
    policy.byte_limit = 64 * 1024;
    let error = db
        .transaction(|tx| {
            let mut material = InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
            material.scope_facts_tx(tx).map(|_| ())
        })
        .err()
        .context("old incarnation must reject before successor State traversal")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::RetiredScope)
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.connection()?.query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM agent_states WHERE agent_id=?1",
            [scope().agent_id],
            |r| r.get::<_, usize>(0)
        )?,
        1024 * 1024 + 1
    );
    Ok(())
}

#[test]
fn tool_scratch_release_requires_its_original_phase_and_last_actual_guard_holder() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let actual_scope = scope();
    let mut first = InputReadMaterial::new(&actual_scope, &request(), &ledger, (0,0))?;
    let mut other = InputReadMaterial::new(&actual_scope, &request(), &ledger, (0,0))?;
    let foreign = first.tool_scratch_mark();
    assert!(other.retire_tool_scratch(foreign, None).unwrap_err().to_string().contains("another Input phase"));
    let before = first.usage();
    let mark = first.tool_scratch_mark();
    let retry = first.tool_scratch_mark();
    first.observe_projected(1024 * 1024, 8)?;
    let last = first.guards.last().context("actual temporary guard missing")?.clone();
    let weak = Arc::downgrade(&last);
    let held = ledger.report()?.observed_bytes;
    assert!(first.retire_tool_scratch(mark, None).unwrap_err().to_string().contains("another live holder"));
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop(last);
    first.retire_tool_scratch(retry, None)?;
    assert!(weak.upgrade().is_none());
    assert_eq!(first.usage(), before);
    drop((first,other));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
