use super::*;
use crate::{
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority,
    },
};
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "material-execution-source".into(),
        identity_incarnation: 1,
    }
}
fn request() -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::from([Key::new(Family::Message, "actual-source")]),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: MAX_RECORDS,
        byte_limit: MAX_METADATA_BYTES,
    }
}
fn fresh() -> Result<(tempfile::TempDir, RuntimeDb, MessageEnvelope)> {
    let home = tempfile::tempdir()?;
    let db =
        RuntimeDb::open_and_migrate(home.path().join("db.sqlite"), home.path().join("db.lock"))?;
    let mut message = MessageEnvelope::new(
        scope().agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "actual original execution source".into(),
        },
    );
    message.id = "actual-source".into();
    message.work_item_id = Some("actual-absent-work".into());
    let actual = db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                scope().agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new(scope().agent_id))?;
        let (actual, inserted) = append_message_tx(tx, &message)?;
        assert!(inserted);
        let delta = register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(actual.clone())],
        )?;
        assert_eq!(delta.records.len(), 1);
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM input_evidence_execution_current_sources",
                [],
                |r| r.get::<_, u64>(0)
            )?,
            1
        );
        assert_eq!(
            tx.query_row(
                "SELECT presence FROM input_evidence_execution_current_work",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "absent"
        );
        Ok(actual)
    })?;
    Ok((home, db, actual))
}
fn observe(
    db: &RuntimeDb,
    request: &InputEvidenceLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<(InputEvidenceBootstrapSnapshot, Vec<Arc<BudgetViewGuard>>)> {
    db.transaction(|tx| {
        let scope = scope();
        let mut material = InputReadMaterial::new(&scope, request, ledger, (0, 0))?;
        let read = prepare_input_evidence_snapshot_with_material_tx(
            tx,
            &scope,
            request,
            false,
            &mut material,
        )?;
        Ok((read, material.into_guards()))
    })
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
fn original_execution_source_raw_and_native_are_admitted_before_metadata_decode() -> Result<()> {
    for native in [false, true] {
        let (_home, db, _actual) = fresh()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let mut policy = request();
        policy.byte_limit = 1024 * 1024;
        if native {
            policy.record_limit = 3500;
        }
        let before = observe(&db, &policy, &ledger)?;
        drop(before);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let original: String = db.connection()?.query_row(
            "SELECT metadata_json FROM input_evidence_execution_current_sources",
            [],
            |r| r.get(0),
        )?;
        let changed = if native {
            let mut value: serde_json::Value = serde_json::from_str(&original)?;
            value["unretained_native_probe"] =
                serde_json::json!(vec![serde_json::Value::Null; 2048]);
            serde_json::to_string(&value)?
        } else {
            "{".to_owned() + &"x".repeat(256 * 1024)
        };
        db.connection()?.execute(
            "UPDATE input_evidence_execution_current_sources SET metadata_json=?1",
            [&changed],
        )?;
        let error = observe(&db, &policy, &ledger)
            .err()
            .context("source material must reject before original digest/decoder")?;
        resource(error);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(
            db.connection()?.query_row(
                "SELECT metadata_json FROM input_evidence_execution_current_sources",
                [],
                |r| r.get::<_, String>(0)
            )?,
            changed
        );
        db.connection()?.execute(
            "UPDATE input_evidence_execution_current_sources SET metadata_json=?1",
            [original],
        )?;
        let repaired = observe(&db, &policy, &ledger)?;
        drop(repaired);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
#[test]
fn execution_work_scalar_and_affinity_source_are_bounded_before_first_owned_projection(
) -> Result<()> {
    for mode in 0..3 {
        let (_home, db, _actual) = fresh()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let mut policy = request();
        policy.byte_limit = 1024 * 1024;
        let baseline = observe(&db, &policy, &ledger)?;
        drop(baseline);
        let large = "x".repeat(256 * 1024);
        db.connection()?.execute(if mode == 0 {
            "UPDATE input_evidence_execution_current_work SET revision=?1 WHERE work_item_id='actual-absent-work'"
        } else if mode == 1 {
            "UPDATE messages SET payload_json=json_set(payload_json,'$.work_item_id',?1) WHERE evidence_id='actual-source'"
        } else {
            "UPDATE messages SET payload_json=?1 WHERE evidence_id='actual-source'"
        },[&large])?;
        let error = observe(&db, &policy, &ledger)
            .err()
            .context("execution Work scalar must be budgeted before String/int projection")?;
        resource(error);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(db.connection()?.query_row(if mode == 0 {
            "SELECT length(CAST(revision AS BLOB)) FROM input_evidence_execution_current_work"
        } else if mode == 1 {
            "SELECT length(CAST(json_extract(payload_json,'$.work_item_id') AS BLOB)) FROM messages WHERE evidence_id='actual-source'"
        } else {
            "SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE evidence_id='actual-source'"
        },[],|r|r.get::<_,usize>(0))?,large.len());
    }
    Ok(())
}
#[test]
fn actual_execution_source_and_absent_work_share_one_phase_through_last_holder() -> Result<()> {
    let (_home, db, actual) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (snapshot, guards) = observe(&db, &request(), &ledger)?;
    let InputEvidenceBootstrapSnapshot::Current(snapshot) = snapshot else {
        anyhow::bail!("actual source retired")
    };
    let captured = snapshot.resident.capture()?;
    let source = captured.execution.source("actual-source")?;
    assert!(source.attempts.is_empty());
    assert_eq!(source.revision, 1);
    assert_eq!(
        captured.execution.work_items["actual-absent-work"].presence,
        crate::runtime::resident_input_evidence::ExecutionWorkPresence::Absent
    );
    let item = captured
        .lookup(&Key::new(Family::Message, "actual-source"))?
        .context("actual qualified Message missing")?;
    assert_eq!(item.record, PreparedInputEvidence::Message(actual));
    assert!(ledger.report()?.observed_bytes > 0);
    let weak = guards.iter().map(Arc::downgrade).collect::<Vec<_>>();
    let held = guards.clone();
    drop((guards, captured, snapshot));
    assert!(ledger.report()?.observed_bytes > 0);
    assert!(weak.iter().all(|g| g.upgrade().is_some()));
    drop(held);
    assert!(weak.iter().all(|g| g.upgrade().is_none()));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
