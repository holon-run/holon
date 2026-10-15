use super::*;
use crate::{
    runtime_db::{repositories::derive_turn_records_from_legacy_evidence, RuntimeDb},
    types::{
        AuthorityClass, BriefKind, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority,
    },
};

type Source = (
    Vec<serde_json::Value>,
    Vec<ToolExecutionRecord>,
    Vec<BriefRecord>,
    Vec<DeliverySummaryRecord>,
    Vec<WaitConditionRecord>,
);
fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}
fn message(id: &str, turn: &str) -> Result<serde_json::Value> {
    let mut message = MessageEnvelope::new(
        "received-agent",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("original-actor".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "received original body".into(),
        },
    );
    message.id = id.into();
    message.turn_id = Some(turn.into());
    message.work_item_id = Some("original-work".into());
    message.created_at = "2026-10-14T01:02:03Z".parse()?;
    serde_json::to_value(message).map_err(Into::into)
}
fn source() -> Result<Source> {
    let at = "2026-10-14T01:02:03Z";
    let tool = serde_json::from_value(serde_json::json!({
        "id":"received-tool","agent_id":"received-agent","turn_id":"received-turn", "turn_index":7,
        "work_item_id":"original-work","tool_name":"Command","created_at":at,
        "authority_class":"operator_instruction","status":"success","input":{},"output":{"stdout":"original"},"summary":"tool",
    }))?;
    let mut brief = BriefRecord::new(
        "received-agent",
        BriefKind::Result,
        "original Brief",
        None,
        None,
    );
    brief.id = "received-brief".into();
    brief.created_at = at.parse()?;
    brief.turn_id = Some("received-turn".into());
    brief.turn_index = Some(7);
    brief.work_item_id = Some("original-work".into());
    let mut delivery = DeliverySummaryRecord::new(
        "received-agent",
        "original-work",
        "legacy delivery",
        Some(7),
        None,
    );
    delivery.id = "received-delivery".into();
    delivery.created_at = at.parse()?;
    delivery.turn_id = Some("received-turn".into());
    let wait = serde_json::from_value(serde_json::json!({
        "id":"received-wait","agent_id":"received-agent","turn_id":"received-turn", "work_item_id":"original-work",
        "status":"active","kind":"external","waiting_for":"original input","created_at":at,"updated_at":at,
    }))?;
    Ok((
        vec![
            message("received-message", "received-turn")?,
            message("received-message", "received-turn")?,
        ],
        vec![tool],
        vec![brief],
        vec![delivery],
        vec![wait],
    ))
}
fn import(db: &RuntimeDb, source: Source) -> Result<()> {
    db.turn_records()
        .import_legacy(source.0, source.1, source.2, source.3, source.4)
}
fn marker(db: &RuntimeDb) -> Result<(String, String, serde_json::Value)> {
    db.connection()?.query_row(
        "SELECT import_status,canonical_source,source_checkpoint_json FROM storage_domains WHERE domain='turn_records'",
        [], |r| Ok((r.get(0)?,r.get(1)?,serde_json::from_str(&r.get::<_,String>(2)?).unwrap())),
    ).map_err(Into::into)
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
fn received_five_families_keep_original_derivation_merge_checkpoint_and_no_input_grant(
) -> Result<()> {
    let (_home, db) = fixture()?;
    let input = source()?;
    let mut expected = derive_turn_records_from_legacy_evidence(
        input.0.clone(),
        input.1.clone(),
        input.2.clone(),
        input.3.clone(),
        input.4.clone(),
    )?
    .remove(0);
    assert_eq!(expected.turn_index, 7);
    assert_eq!(expected.input_message_ids, ["received-message"]);
    // The original existing Turn owns its immutable trigger/birth/metadata;
    // imported IDs merge and dedup exactly as before this material wrapper.
    let mut existing = expected.clone();
    existing.input_message_ids.push("already-original".into());
    existing.produced_brief_ids.clear();
    existing.tool_execution_ids.clear();
    db.turn_records().upsert(&existing)?;
    expected.input_message_ids.push("already-original".into());
    expected.input_message_ids.sort();
    import(&db, input)?;
    assert_eq!(
        db.turn_records()
            .by_id(Some("received-agent"), "received-turn")?,
        Some(expected)
    );
    let (status, canonical, checkpoint) = marker(&db)?;
    assert_eq!(status, "complete");
    assert_eq!(canonical, "db");
    assert_eq!(
        checkpoint,
        serde_json::json!({"imported_records":1,"source":"legacy evidence jsonl","ignored":"turns.jsonl"})
    );
    let members: usize =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| {
                r.get(0)
            })?;
    assert_eq!(members, 0);
    // An already complete received actor does not recapture a later source.
    import(
        &db,
        (
            vec![serde_json::json!({"not":"a valid native message"})],
            vec![],
            vec![],
            vec![],
            vec![],
        ),
    )?;
    assert_eq!(marker(&db)?.2, checkpoint);
    Ok(())
}

#[test]
fn received_source_and_actual_derived_metadata_each_fit_but_original_joint_actor_rejects(
) -> Result<()> {
    let (_home, db) = fixture()?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut input: Source = (vec![], vec![], vec![], vec![], vec![]);
    // Use the actual borrowed source census and original observer to locate
    // a both-individually-fit, jointly-overflowing boundary under unchanged caps.
    let mut joint_boundary = false;
    for index in 0..512 {
        input.0.push(message(
            &format!("received-{index}"),
            &format!("turn-{index}"),
        )?);
        let source_only =
            ReceivedTurnMaterial::new(&scratch, &input.0, &input.1, &input.2, &input.3, &input.4)?;
        let source_nodes = source_only.source.as_ref().unwrap().records;
        drop(source_only);
        let mut derived_bytes = 0u64;
        let mut derived_nodes = 0usize;
        let derived =
            crate::runtime_db::repositories::derive_turn_records_from_legacy_evidence_observed(
                input.0.clone(),
                vec![],
                vec![],
                vec![],
                vec![],
                Some(&mut |bytes, nodes| {
                    derived_bytes += bytes;
                    derived_nodes += nodes;
                    Ok(())
                }),
            )?;
        assert_eq!(derived.len(), input.0.len());
        assert!(derived_bytes < MAX_METADATA_BYTES as u64 && derived_nodes < MAX_RECORDS);
        assert_eq!(scratch.report()?.observed_bytes, 0);
        if source_nodes + derived_nodes > MAX_RECORDS {
            joint_boundary = true;
            break;
        }
    }
    assert!(joint_boundary, "no true joint native boundary found");
    let error = import(&db, input).unwrap_err();
    resource(&error);
    assert_eq!(marker(&db)?.0, "failed");
    let rows: usize = db
        .connection()?
        .query_row("SELECT COUNT(*) FROM turn_records", [], |r| r.get(0))?;
    assert_eq!(rows, 0);
    assert_eq!(scratch.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn received_wait_native_and_spare_vec_slots_are_counted_before_normalizer_or_turn_write(
) -> Result<()> {
    for spare_slots in [false, true] {
        let (_home, db) = fixture()?;
        let mut input = source()?;
        if spare_slots {
            input.4.reserve_exact(MAX_RECORDS);
            assert!(input.4.capacity() >= MAX_RECORDS);
        } else {
            input.4[0].continuation = Some(serde_json::Value::Object(
                (0..5000)
                    .map(|index| (format!("small-{index}"), serde_json::Value::Null))
                    .collect(),
            ));
            assert!(serde_json::to_vec(&input.4)?.len() < 1024 * 1024);
        }
        let error = import(&db, input).unwrap_err();
        resource(&error);
        assert_eq!(marker(&db)?.0, "failed");
        let rows: usize =
            db.connection()?
                .query_row("SELECT COUNT(*) FROM turn_records", [], |r| r.get(0))?;
        assert_eq!(rows, 0);
    }
    Ok(())
}

#[test]
fn received_existing_turn_raw_native_and_foreign_conflicts_keep_atomic_failed_retry() -> Result<()>
{
    for kind in ["raw", "native", "foreign"] {
        let (_home, db) = fixture()?;
        let mut input = source()?;
        let mut existing = derive_turn_records_from_legacy_evidence(
            input.0.clone(),
            input.1.clone(),
            input.2.clone(),
            input.3.clone(),
            input.4.clone(),
        )?
        .remove(0);
        if kind == "foreign" {
            existing.agent_id = "other-owner".into();
            existing.input_message_ids.clear();
        }
        db.turn_records().upsert(&existing)?;
        let before_assignments: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM conversation_input_assignments",
            [],
            |r| r.get(0),
        )?;
        let original: String = db.connection()?.query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id='received-turn'",
            [],
            |r| r.get(0),
        )?;
        if kind != "foreign" {
            let bad = if kind == "raw" {
                "[".to_owned() + &"x".repeat(9 * 1024 * 1024)
            } else {
                let mut value: serde_json::Value = serde_json::from_str(&original)?;
                value["extra"] = serde_json::Value::Object(
                    (0..9000)
                        .map(|index| (format!("node-{index}"), serde_json::Value::Null))
                        .collect(),
                );
                serde_json::to_string(&value)?
            };
            db.connection()?.execute(
                "UPDATE turn_records SET payload_json=?1 WHERE turn_id='received-turn'",
                [bad],
            )?;
        }
        // Sorted before the failing original Turn: its write/assignment must
        // roll back when the later original merge/raw/native gate fails.
        input
            .0
            .push(message("before-error-message", "aaa-before-error")?);
        let error = import(&db, input).unwrap_err();
        if kind == "foreign" {
            assert!(error
                .to_string()
                .contains("importing legacy storage domain turn_records"));
            assert!(format!("{error:#}").contains("turn identity"));
        } else {
            resource(&error);
        }
        assert_eq!(marker(&db)?.0, "failed");
        let rows: usize =
            db.connection()?
                .query_row("SELECT COUNT(*) FROM turn_records", [], |r| r.get(0))?;
        assert_eq!(rows, 1);
        let assignments: usize = db.connection()?.query_row(
            "SELECT COUNT(*) FROM conversation_input_assignments",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(assignments, before_assignments);
        db.connection()?.execute("UPDATE turn_records SET agent_id='received-agent',payload_json=?1 WHERE turn_id='received-turn'",[&original])?;
        if kind == "foreign" {
            existing.agent_id = "received-agent".into();
            db.connection()?.execute(
                "UPDATE turn_records SET payload_json=?1 WHERE turn_id='received-turn'",
                [serde_json::to_string(&existing)?],
            )?;
        }
        import(&db, source()?)?;
        assert_eq!(marker(&db)?.0, "complete");
    }
    Ok(())
}

#[test]
fn received_source_guard_retires_after_consume_and_derived_row_guards_release_last() -> Result<()> {
    let input = source()?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut material =
        ReceivedTurnMaterial::new(&scratch, &input.0, &input.1, &input.2, &input.3, &input.4)?;
    let records =
        crate::runtime_db::repositories::derive_turn_records_from_legacy_evidence_observed(
            input.0,
            input.1,
            input.2,
            input.3,
            input.4,
            Some(&mut |bytes, nodes| material.observe_derived(bytes, nodes)),
        )?;
    let peak = scratch.report()?.observed_bytes;
    material.source_consumed();
    assert!(scratch.report()?.observed_bytes < peak);
    let retained = scratch.report()?.observed_bytes;
    let (_home, db) = fixture()?;
    db.transaction(|tx| {
        for record in &records {
            let mut row = material.existing_turn_tx(tx, &record.turn_id)?;
            material.assignment_material_tx(tx, record, &mut row)?;
            assert!(scratch.report()?.observed_bytes > retained);
            drop(row);
            assert_eq!(scratch.report()?.observed_bytes, retained);
        }
        Ok(())
    })?;
    let last = material.derived.guards.last().unwrap().clone();
    let weak = Arc::downgrade(&last);
    drop((records, material));
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(scratch.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn received_assignment_raw_precedes_original_string_and_normalizer_errors_keep_checkpoint(
) -> Result<()> {
    let (_home, db) = fixture()?;
    let large = "old-turn-".to_owned() + &"x".repeat(9 * 1024 * 1024);
    db.connection()?.execute(
        "INSERT INTO conversation_input_assignments(message_id,agent_id,turn_id,revision,assigned_at) VALUES('received-message','received-agent',?1,1,0)",
        [&large],
    )?;
    let error = import(&db, source()?).unwrap_err();
    resource(&error);
    assert_eq!(marker(&db)?.0, "failed");
    let rows: usize = db
        .connection()?
        .query_row("SELECT COUNT(*) FROM turn_records", [], |r| r.get(0))?;
    assert_eq!(rows, 0);
    let bytes:usize=db.connection()?.query_row("SELECT length(CAST(turn_id AS BLOB)) FROM conversation_input_assignments WHERE message_id='received-message'",[],|r|r.get(0))?;
    assert_eq!(bytes, large.len());
    db.connection()?.execute(
        "DELETE FROM conversation_input_assignments WHERE message_id='received-message'",
        [],
    )?;
    import(&db, source()?)?;
    assert_eq!(marker(&db)?.0, "complete");

    let (_home2, db2) = fixture()?;
    let mut input = source()?;
    let bad_kind = "unsupported-original-kind-".to_owned() + &"k".repeat(64 * 1024);
    input.0[0]["kind"] = serde_json::Value::String(bad_kind.clone());
    let error = import(&db2, input).unwrap_err();
    assert!(format!("{error:#}").contains("unknown variant"));
    let (_, _, checkpoint) = marker(&db2)?;
    let detail = checkpoint["error"]
        .as_str()
        .context("original failed checkpoint omitted error")?;
    assert!(detail.contains(&bad_kind));
    assert_eq!(
        checkpoint["retry"],
        "restart runtime to retry legacy import"
    );
    let rows: usize =
        db2.connection()?
            .query_row("SELECT COUNT(*) FROM turn_records", [], |r| r.get(0))?;
    assert_eq!(rows, 0);
    import(&db2, source()?)?;
    assert_eq!(marker(&db2)?.0, "complete");
    Ok(())
}
