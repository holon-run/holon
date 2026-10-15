use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        evidence::{
            insert_delivery_summary_evidence_tx, insert_message_evidence_tx,
            upsert_agent_identity_tx,
        },
        repositories::{upsert_turn_record_tx, upsert_wait_condition_tx},
    },
    types::*,
};

fn fixture() -> Result<(tempfile::TempDir, AppStorage)> {
    let home = tempfile::tempdir()?;
    let storage = AppStorage::new_for_test_with_agent(home.path(), "original")?;
    storage.runtime_db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                "original",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )
    })?;
    Ok((home, storage))
}
fn source(storage: &AppStorage) -> OriginalEpisodeImport<'_> {
    OriginalEpisodeImport {
        agent_id: storage.agent_id.as_deref().unwrap(),
    }
}
fn delivery(id: &str) -> DeliverySummaryRecord {
    let mut record =
        DeliverySummaryRecord::new("original", "work-first", "complete", Some(7), None);
    record.id = id.into();
    record.turn_id = Some("turn-stream".into());
    record.created_at = "2026-10-14T01:00:00.000000123Z".parse().unwrap();
    record
}
fn message(id: &str, turn: &str) -> MessageEnvelope {
    let mut record = MessageEnvelope::new(
        "original",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original input".into(),
        },
    );
    record.id = id.into();
    record.turn_id = Some(turn.into());
    record.message_seq = Some(2);
    record.created_at = "2026-10-14T01:00:00.000000321Z".parse().unwrap();
    record
}
fn wait(id: &str) -> WaitConditionRecord {
    let at = "2026-10-14T00:59:00.000000789Z".parse().unwrap();
    WaitConditionRecord {
        id: id.into(),
        agent_id: "original".into(),
        work_item_id: Some("wait-work".into()),
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::External,
        source: Some("original".into()),
        subject_ref: None,
        waiting_for: "original wait".into(),
        wake_sources: vec![WakeSource::ExternalIngress {
            external_trigger_id: None,
        }],
        continuation: None,
        created_at: at,
        updated_at: at,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: Some("turn-stream".into()),
        trigger_message_id: None,
        triggered_at: None,
    }
}

#[test]
fn actual_original_storage_import_streams_100001_turn_links_and_evidence_atomically_per_domain(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    assert!(!db.storage_domain_is_complete("turn_records", "db")?);
    assert!(!db.storage_domain_is_complete("evidence", "db")?);
    db.transaction(|tx| {
        for index in 0..100_001 {
            insert_delivery_summary_evidence_tx(tx, &delivery(&format!("delivery-{index:06}")))?;
        }
        Ok(())
    })?;
    // The actual first caller executes both original business transactions.
    storage.legacy_importer().import_episode_ref_domains()?;
    let (turns,links,work,source_members,evidence_members):(u64,u64,u64,u64,u64)=db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM turn_records),(SELECT json_array_length(payload_json,'$.delivery_summary_ids') FROM turn_records WHERE turn_id='turn-stream'),(SELECT json_array_length(payload_json,'$.completed_work_item_ids') FROM turn_records WHERE turn_id='turn-stream'),(SELECT member_count FROM legacy_episode_import_captures),(SELECT member_count FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/actual_evidence_import')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    assert_eq!(
        (turns, links, work, source_members, evidence_members),
        (1, 100_001, 1, 100_001, 100_001)
    );
    let ends:(String,String)=db.connection()?.query_row("SELECT json_extract(payload_json,'$.delivery_summary_ids[0]'),json_extract(payload_json,'$.delivery_summary_ids[100000]') FROM turn_records WHERE turn_id='turn-stream'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!(ends, ("delivery-000000".into(), "delivery-100000".into()));
    for domain in ["turn_records", "evidence"] {
        assert!(db.storage_domain_is_complete(domain, "db")?);
    }
    let aliases: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE family='legacy_delivery'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(aliases, 0);
    let before:(u64,u64)=db.connection()?.query_row("SELECT summary_revision,detail_revision FROM conversation_turn_revisions WHERE agent_id='original' AND turn_id='turn-stream'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    storage.legacy_importer().import_episode_ref_domains()?;
    let after:(u64,u64)=db.connection()?.query_row("SELECT summary_revision,detail_revision FROM conversation_turn_revisions WHERE agent_id='original' AND turn_id='turn-stream'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!(before, after);
    Ok(())
}

#[test]
fn original_turn_stream_scratch_is_bounded_across_100001_links_and_released_after_complete(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    db.transaction(|tx| {
        for index in 0..100_001 {
            insert_delivery_summary_evidence_tx(tx, &delivery(&format!("delivery-{index:06}")))?;
        }
        Ok(())
    })?;
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    db.freeze_original_turn_wait_source(&source)?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    let peak = db.import_original_turn_records_with_scratch_for_test(&source, &scratch)?;
    assert!(
        peak > 65_536 && peak < 2 * 1024 * 1024,
        "owned Rust scratch={peak}; not SQLite or RSS"
    );
    assert_eq!(scratch.report()?.observed_bytes, 0);
    let links:u64=db.connection()?.query_row("SELECT json_array_length(payload_json,'$.delivery_summary_ids') FROM turn_records WHERE turn_id='turn-stream'",[],|r|r.get(0))?;
    assert_eq!(links, 100_001);
    assert!(db.storage_domain_is_complete("turn_records", "db")?);
    Ok(())
}

fn parity_source(storage: &AppStorage, existing: bool) -> Result<()> {
    let db = &storage.runtime_db;
    let ids = ["z", "é", "中", "\u{0}", "line\n\t\"\\", "\u{1}", "\u{2028}"];
    for id in ids {
        let mut record = delivery(id);
        record.work_item_id = format!("work-{id}");
        db.evidence().append_delivery_summary(&record)?;
    }
    let mut m = message("m-original", "turn-stream");
    m.work_item_id = Some("message-first-work".into());
    db.transaction(|tx| insert_message_evidence_tx(tx, &m))?;
    let tool = ToolExecutionRecord {
        id: "tool-original".into(),
        agent_id: "original".into(),
        work_item_id: Some("tool-work".into()),
        turn_index: 7,
        turn_id: Some("turn-stream".into()),
        tool_name: "original".into(),
        created_at: m.created_at,
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::Value::Null,
        output: serde_json::Value::Null,
        summary: "original".into(),
        invocation_surface: None,
    };
    db.evidence().append_tool_execution(&tool)?;
    let mut brief = BriefRecord::new("original", BriefKind::Ack, "original", None, None);
    brief.id = "brief-original".into();
    brief.turn_id = Some("turn-stream".into());
    brief.turn_index = Some(7);
    brief.created_at = m.created_at;
    db.evidence().append_brief(&brief)?;
    db.transaction(|tx| upsert_wait_condition_tx(tx, &wait("wait-original")).map(|_| ()))?;
    // The first second Turn exercises BTreeMap ordering, not source insertion.
    let mut other = message("other-input", "aaa-turn");
    other.message_seq = Some(3);
    db.transaction(|tx| insert_message_evidence_tx(tx, &other))?;
    if existing {
        let mut turn = TurnRecord::new("original", "turn-stream", 11);
        turn.created_at = "2026-10-13T01:00:00.000000987Z".parse()?;
        turn.run_id = Some("existing-run".into());
        turn.owner = Some(TurnOwner::Command);
        turn.trigger = Some(TurnTriggerSummary::from_message(&message(
            "existing-trigger",
            "turn-stream",
        )));
        turn.input_message_ids = vec!["prior-input".into()];
        turn.delivery_summary_ids = vec!["z".into(), "prior-delivery".into()];
        turn.terminal = Some(TurnTerminalSummary {
            kind: TurnTerminalKind::Completed,
            reason: Some("original terminal".into()),
            no_brief_reason: Some(TurnNoBriefReason::ToolOnlyWait),
            completed_at: turn.created_at + chrono::Duration::seconds(1),
            duration_ms: 1000,
        });
        turn.destructive_operation_id = Some("original-destructive-fence".into());
        turn.external_side_effect_possible = true;
        turn.replay = Some(TurnReplayProvenance {
            source_message_id: "prior-input".into(),
            source_turn_id: "prior-source".into(),
            reason: "original recovery".into(),
            prior_terminal: turn.terminal.clone(),
        });
        db.transaction(|tx| {
            let mut prior = TurnRecord::new("original", "prior-source", 1);
            prior.created_at = turn.created_at;
            prior.input_message_ids = vec!["prior-input".into()];
            upsert_turn_record_tx(tx, &prior)?;
            // Exercise the original replay source-owns check, not only an
            // already assigned shortcut. It restores the same source owner.
            tx.execute(
                "DELETE FROM conversation_input_assignments WHERE message_id='prior-input'",
                [],
            )?;
            upsert_turn_record_tx(tx, &turn)
        })?;
    }
    Ok(())
}
#[test]
fn original_turn_stream_matches_received_derive_merge_upsert_bytes_columns_and_conversation_bookkeeping(
) -> Result<()> {
    for existing in [false, true] {
        let (_a, stream) = fixture()?;
        let (_b, baseline) = fixture()?;
        parity_source(&stream, existing)?;
        parity_source(&baseline, existing)?;
        baseline.runtime_db.turn_records().import_legacy(
            baseline.read_all_message_values()?,
            baseline.read_recent_tool_executions(usize::MAX)?,
            baseline.read_recent_briefs(usize::MAX)?,
            baseline.read_recent_delivery_summaries(usize::MAX)?,
            baseline.read_recent_wait_conditions(usize::MAX)?,
        )?;
        let source = source(&stream);
        stream.runtime_db.freeze_original_episode_import(&source)?;
        stream
            .runtime_db
            .freeze_original_turn_wait_source(&source)?;
        stream.runtime_db.import_original_turn_records(&source)?;
        for turn in ["aaa-turn", "turn-stream"] {
            let raw = |storage: &AppStorage| -> Result<String> {
                Ok(storage.runtime_db.connection()?.query_row(
                    "SELECT payload_json FROM turn_records WHERE turn_id=?1",
                    [turn],
                    |r| r.get(0),
                )?)
            };
            assert_eq!(
                raw(&stream)?,
                raw(&baseline)?,
                "raw Serde parity existing={existing},turn={turn}"
            );
            let record = stream
                .runtime_db
                .turn_records()
                .by_id(Some("original"), turn)?
                .unwrap();
            assert_eq!(
                Some(record),
                baseline
                    .runtime_db
                    .turn_records()
                    .by_id(Some("original"), turn)?
            );
        }
        for (table, order) in [
            ("turn_records", "turn_id"),
            ("conversation_turn_revisions", "agent_id,turn_id"),
            ("conversation_input_assignments", "message_id"),
            ("conversation_source_revisions", "source_kind,source_id"),
        ] {
            let columns = |storage: &AppStorage| -> Result<Vec<Vec<rusqlite::types::Value>>> {
                let connection = storage.runtime_db.connection()?;
                let mut statement =
                    connection.prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))?;
                let width = statement.column_count();
                let rows = statement.query_map([], |r| {
                    (0..width)
                        .map(|i| r.get(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
            };
            assert_eq!(
                columns(&stream)?,
                columns(&baseline)?,
                "all normalized columns {table},existing={existing}"
            );
        }
    }
    Ok(())
}

#[test]
fn original_turn_version_failure_rolls_back_all_turns_and_assignments_then_reuses_exact_cohort(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    db.transaction(|tx| {
        insert_message_evidence_tx(tx, &message("first-input", "aaa-turn"))?;
        insert_delivery_summary_evidence_tx(tx, &delivery("second-source"))?;
        Ok(())
    })?;
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    db.freeze_original_turn_wait_source(&source)?;
    let original: String = db.connection()?.query_row(
        "SELECT payload_json FROM delivery_summaries WHERE evidence_id='second-source'",
        [],
        |r| r.get(0),
    )?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE delivery_summaries SET payload_json='{}' WHERE evidence_id='second-source'",
            [],
        )?;
        Ok(())
    })?;
    let error = db.import_original_turn_records(&source).unwrap_err();
    assert!(
        format!("{error:#}").contains("source version changed"),
        "{error:#}"
    );
    let counts:(u64,u64,u64)=db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM turn_records),(SELECT COUNT(*) FROM conversation_input_assignments),(SELECT COUNT(*) FROM sqlite_temp_master WHERE name LIKE 'original_turn_import_%')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!(counts, (0, 0, 0));
    assert!(!db.storage_domain_is_complete("turn_records", "db")?);
    let status: String = db.connection()?.query_row(
        "SELECT import_status FROM storage_domains WHERE domain='turn_records'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(status, "failed");
    db.transaction(|tx| {
        tx.execute(
            "UPDATE delivery_summaries SET payload_json=?1 WHERE evidence_id='second-source'",
            [&original],
        )?;
        insert_delivery_summary_evidence_tx(tx, &delivery("late-raw"))?;
        Ok(())
    })?;
    db.import_original_turn_records(&source)?;
    let (count,late):(u64,bool)=db.connection()?.query_row("SELECT (SELECT COUNT(*) FROM turn_records),EXISTS(SELECT 1 FROM turn_records,json_each(payload_json,'$.delivery_summary_ids') WHERE value='late-raw')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!((count, late), (2, false));
    Ok(())
}

#[test]
fn original_turn_wait_source_counts_normalized_nodes_before_decoder_and_never_reads_payload_json(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    db.transaction(|tx| upsert_wait_condition_tx(tx, &wait("wait-original")).map(|_| ()))?;
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    db.transaction(|tx|{tx.execute("UPDATE wait_conditions SET continuation_json=?1,payload_json='{' WHERE wait_condition_id='wait-original'",[serde_json::to_string(&vec![serde_json::Value::Null;9000])?])?;Ok(())})?;
    let error = db.freeze_original_turn_wait_source(&source).unwrap_err();
    assert!(
        format!("{error:#}").contains("original episode source scratch"),
        "{error:#}"
    );
    let count: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM legacy_episode_wait_captures",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    db.transaction(|tx|{tx.execute("UPDATE wait_conditions SET continuation_json=NULL WHERE wait_condition_id='wait-original'",[])?;Ok(())})?;
    // payload_json deliberately remains malformed: the old actual Wait reader
    // and this source both derive from normalized columns only.
    db.freeze_original_turn_wait_source(&source)?;
    db.import_original_turn_records(&source)?;
    let count:u64=db.connection()?.query_row("SELECT json_array_length(payload_json,'$.waiting_condition_ids') FROM turn_records WHERE turn_id='turn-stream'",[],|r|r.get(0))?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn original_turn_stream_keeps_foreign_scope_and_mixed_existing_arrays_rejected() -> Result<()> {
    for foreign in [false, true] {
        let (_home, storage) = fixture()?;
        let db = &storage.runtime_db;
        db.evidence().append_delivery_summary(&delivery("source"))?;
        let mut original = TurnRecord::new(
            if foreign { "foreign" } else { "original" },
            "turn-stream",
            7,
        );
        original.created_at = "2026-10-13T01:00:00Z".parse()?;
        db.transaction(|tx| upsert_turn_record_tx(tx, &original))?;
        if !foreign {
            db.transaction(|tx|{tx.execute("UPDATE turn_records SET payload_json=json_set(payload_json,'$.input_message_ids',json('[1]')) WHERE turn_id='turn-stream'",[])?;Ok(())})?;
        }
        let before: String = db.connection()?.query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id='turn-stream'",
            [],
            |r| r.get(0),
        )?;
        let source = source(&storage);
        db.freeze_original_episode_import(&source)?;
        db.freeze_original_turn_wait_source(&source)?;
        let error = db.import_original_turn_records(&source).unwrap_err();
        assert!(
            format!("{error:#}").contains(if foreign {
                "normalized identity differs"
            } else {
                "ID array is not text"
            }),
            "{error:#}"
        );
        let after: String = db.connection()?.query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id='turn-stream'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(before, after);
        assert!(!db.storage_domain_is_complete("turn_records", "db")?);
    }
    Ok(())
}

#[test]
fn original_turn_v81_unbound_seed_imports_but_a_new_incarnation_cannot_claim_it() -> Result<()> {
    use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
    for changed in [false, true] {
        let home = tempfile::tempdir()?;
        let path = home.path().join("old.sqlite");
        {
            let mut connection = rusqlite::Connection::open(&path)?;
            ensure_migration_table(&connection)?;
            for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
                apply_migration(&mut connection, migration)?;
            }
            let tx = connection.transaction()?;
            crate::runtime_db::evidence::upsert_agent_state_tx(&tx, &AgentState::new("original"))?;
            insert_delivery_summary_evidence_tx(&tx, &delivery("old-delivery"))?;
            tx.commit()?;
        }
        let db = RuntimeDb::open_and_migrate(path, home.path().join("old.lock"))?;
        let storage = AppStorage::new_for_agent(home.path(), "original", db.clone())?;
        let source = source(&storage);
        db.freeze_original_episode_import(&source)?;
        db.freeze_original_turn_wait_source(&source)?;
        if changed {
            // Adversarial later materialization, not a public Host replacement.
            db.transaction(|tx| {
                upsert_agent_identity_tx(
                    tx,
                    &AgentIdentityRecord::new(
                        "original",
                        AgentKind::Named,
                        AgentVisibility::Public,
                        AgentOwnership::SelfOwned,
                        AgentProfilePreset::PublicNamed,
                        None,
                        None,
                    ),
                )
            })?;
            let error = db.import_original_turn_records(&source).unwrap_err();
            assert!(
                format!("{error:#}").contains("incarnation changed"),
                "{error:#}"
            );
            let count: u64 =
                db.connection()?
                    .query_row("SELECT COUNT(*) FROM turn_records", [], |r| r.get(0))?;
            assert_eq!(count, 0);
            assert!(!db.storage_domain_is_complete("turn_records", "db")?);
        } else {
            storage.legacy_importer().import_episode_ref_domains()?;
            assert!(db.storage_domain_is_complete("turn_records", "db")?);
            assert!(db.storage_domain_is_complete("evidence", "db")?);
            let (unbound,count):(bool,u64)=db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_import_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id WHERE c.identity_incarnation IS NULL AND s.identity_incarnation IS NULL AND s.source_kind='upgrade_unbound'),(SELECT json_array_length(payload_json,'$.delivery_summary_ids') FROM turn_records WHERE turn_id='turn-stream')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!((unbound, count), (true, 1));
        }
    }
    Ok(())
}

#[test]
fn original_turn_foreign_assignment_conflict_is_not_reassigned_and_whole_business_tx_rolls_back(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    db.transaction(|tx| {
        insert_message_evidence_tx(tx,&message("original-input","turn-stream"))?;
        let mut foreign=TurnRecord::new("foreign","foreign-turn",1);
        foreign.created_at="2026-10-13T01:00:00Z".parse()?;upsert_turn_record_tx(tx,&foreign)?;
        // Corrupt normalized assignment is an explicit adversarial fixture.
        tx.execute("INSERT INTO conversation_input_assignments(message_id,agent_id,turn_id,revision,assigned_at) VALUES('original-input','foreign','foreign-turn',1,'2026-10-13T01:00:00Z')",[])?;Ok(())
    })?;
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    db.freeze_original_turn_wait_source(&source)?;
    let error = db.import_original_turn_records(&source).unwrap_err();
    assert!(
        format!("{error:#}").contains("assignment identity conflict"),
        "{error:#}"
    );
    let (target,count):(String,u64)=db.connection()?.query_row("SELECT (SELECT turn_id FROM conversation_input_assignments WHERE message_id='original-input'),(SELECT COUNT(*) FROM turn_records WHERE agent_id='original')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!((target, count), ("foreign-turn".into(), 0));
    assert!(!db.storage_domain_is_complete("turn_records", "db")?);
    Ok(())
}
