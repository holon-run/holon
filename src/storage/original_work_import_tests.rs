use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        evidence::{
            insert_brief_evidence_tx, insert_message_evidence_tx, insert_transcript_evidence_tx,
            upsert_agent_identity_tx, upsert_agent_state_tx,
        },
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
    },
    types::*,
};

fn at() -> chrono::DateTime<chrono::Utc> {
    "2026-10-14T01:00:00.000000123Z".parse().unwrap()
}
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
fn brief(id: &str) -> BriefRecord {
    let mut record = BriefRecord::new("original", BriefKind::Result, "accepted result", None, None);
    record.id = id.into();
    record.created_at = at();
    record.work_item_id = Some("work-original".into());
    // No Turn relation: this fixture isolates full Work metadata from the
    // separately tested original Turn's complete linked-ID arrays.
    record
}
fn seed_parity(storage: &AppStorage) -> Result<()> {
    storage.runtime_db.transaction(|tx| {
        let mut message = MessageEnvelope::new(
            "original",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "actual original input".into(),
            },
        );
        message.id = "message:é\u{0}\n".into();
        message.message_seq = Some(1);
        message.created_at = at();
        insert_message_evidence_tx(tx, &message)?;
        let mut transcript = TranscriptEntry::new(
            "original",
            TranscriptEntryKind::AssistantRound,
            Some(1),
            None,
            serde_json::json!({"text":"original body"}),
        );
        transcript.id = "transcript:中\u{1}".into();
        transcript.created_at = at();
        insert_transcript_evidence_tx(tx, &transcript)?;
        for id in ["z", "é", "中", "\u{0}", "line\n\t\"\\", "\u{1}", "\u{2028}"] {
            let record = brief(id);
            insert_brief_evidence_tx(tx, &record)?;
            let pretty = serde_json::to_string_pretty(&record)?;
            tx.execute(
                "UPDATE briefs SET payload_json=?1,content_hash=?2 WHERE evidence_id=?3",
                rusqlite::params![
                    pretty,
                    crate::runtime_db::evidence::content_hash(&pretty),
                    id
                ],
            )?;
        }
        Ok(())
    })
}

#[test]
fn original_work_footprint_matches_received_v1_full_keys_final_digests_and_origins() -> Result<()> {
    let (_a, stream) = fixture()?;
    let (_b, baseline) = fixture()?;
    seed_parity(&stream)?;
    seed_parity(&baseline)?;
    baseline.runtime_db.turn_records().import_legacy(
        baseline.read_all_message_values()?,
        Vec::new(),
        baseline.read_recent_briefs(usize::MAX)?,
        Vec::new(),
        Vec::new(),
    )?;
    baseline.runtime_db.evidence().import_legacy(
        baseline.read_all_message_values()?,
        baseline.read_all_transcript()?,
        Vec::new(),
        baseline.read_recent_briefs(usize::MAX)?,
        Vec::new(),
    )?;
    // Both original domains are genuinely incomplete at this actual caller.
    assert!(!stream
        .runtime_db
        .storage_domain_is_complete("turn_records", "db")?);
    assert!(!stream
        .runtime_db
        .storage_domain_is_complete("evidence", "db")?);
    stream.legacy_importer().import_episode_ref_domains()?;
    let checkpoint = |storage: &AppStorage| -> Result<serde_json::Value> {
        let raw: String = storage.runtime_db.connection()?.query_row(
            "SELECT source_checkpoint_json FROM storage_domains WHERE domain='evidence'",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str::<serde_json::Value>(&raw)?["work_domain_actual_writes"].clone())
    };
    let expected = checkpoint(&baseline)?;
    let actual = checkpoint(&stream)?;
    assert_eq!(actual, expected);
    assert_eq!(actual["records"].as_object().unwrap().len(), 9);
    assert_eq!(
        actual["records"]["brief:\u{0}"]["origin"],
        "legacy_import_equal"
    );
    assert_eq!(
        actual["records"]["message:message:é\u{0}\n"]["origin"],
        "actual_write"
    );
    for table in ["messages", "transcript_entries", "briefs"] {
        let rows = |storage: &AppStorage| -> Result<Vec<(String, String)>> {
            let connection = storage.runtime_db.connection()?;
            let mut statement = connection.prepare(&format!(
                "SELECT evidence_id,payload_json FROM {table} ORDER BY evidence_id COLLATE BINARY"
            ))?;
            let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.map(|row| row.map_err(Into::into)).collect()
        };
        assert_eq!(rows(&stream)?, rows(&baseline)?);
    }
    let before = actual;
    stream.legacy_importer().import_episode_ref_domains()?;
    assert_eq!(checkpoint(&stream)?, before);
    Ok(())
}

#[test]
fn actual_original_work_footprint_100001_rows_has_bounded_scratch_and_complete_v1() -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    db.transaction(|tx| {
        for index in 0..100_001 {
            insert_brief_evidence_tx(tx, &brief(&format!("brief-{index:06}")))?;
        }
        Ok(())
    })?;
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    let peak = db.import_original_episode_evidence_with_scratch_for_test(&source, &scratch)?;
    assert!(
        peak > 65_536 && peak < 2 * 1024 * 1024,
        "owned Rust scratch={peak}, not SQLite/RSS"
    );
    assert_eq!(scratch.report()?.observed_bytes, 0);
    assert!(db.storage_domain_is_complete("evidence", "db")?);
    let (count,version,late):(u64,u64,bool)=db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM storage_domains d,json_each(d.source_checkpoint_json,'$.work_domain_actual_writes.records') WHERE d.domain='evidence'),json_extract(source_checkpoint_json,'$.work_domain_actual_writes.policy_version'),json_type(source_checkpoint_json,'$.work_domain_actual_writes.records.\"brief:late-raw\"') IS NOT NULL FROM storage_domains WHERE domain='evidence'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!((count, version, late), (100_001, 1, false));
    db.evidence().append_brief(&brief("late-raw"))?;
    db.import_original_episode_evidence(&source)?;
    let count:u64=db.connection()?.query_row("SELECT COUNT(*) FROM storage_domains d,json_each(d.source_checkpoint_json,'$.work_domain_actual_writes.records') WHERE d.domain='evidence'",[],|r|r.get(0))?;
    assert_eq!(count, 100_001);
    let temp: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM sqlite_temp_master WHERE name LIKE 'original_work_import_%'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(temp, 0);
    Ok(())
}

#[test]
fn true_v81_original_caller_imports_100001_work_footprints_then_first_context_import_without_whole_map(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("old.sqlite");
    {
        let mut connection = rusqlite::Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
            apply_migration(&mut connection, migration)?;
        }
        let tx = connection.transaction()?;
        upsert_agent_state_tx(&tx, &AgentState::new("original"))?;
        for index in 0..100_001 {
            insert_brief_evidence_tx(&tx, &brief(&format!("brief-{index:06}")))?;
        }
        tx.commit()?;
    }
    let db = RuntimeDb::open_and_migrate(path, home.path().join("old.lock"))?;
    let storage = AppStorage::new_for_agent(home.path(), "original", db.clone())?;
    assert!(!db.storage_domain_is_complete("turn_records", "db")?);
    assert!(!db.storage_domain_is_complete("evidence", "db")?);
    // Execute the actual original reference caller, then the actual first
    // context import. The fixed namespace has no owning Work body to expand.
    storage.legacy_importer().import_episode_ref_domains()?;
    db.context_episodes().import_legacy(Vec::new())?;
    let (full,seed,first,temps,aliases):(u64,u64,u64,u64,u64)=db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM storage_domains d,json_each(d.source_checkpoint_json,'$.work_domain_actual_writes.records') WHERE d.domain='evidence'),(SELECT COUNT(*) FROM episode_continuity_seeds WHERE agent_id='original' AND source_kind='upgrade_unbound'),(SELECT COUNT(*) FROM episode_continuity_imports),(SELECT COUNT(*) FROM sqlite_temp_master WHERE name LIKE 'original_work_import_%'),(SELECT COUNT(*) FROM input_evidence_current WHERE family='brief')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    assert_eq!((full, seed, first, temps, aliases), (100_001, 1, 1, 0, 0));
    assert!(db.storage_domain_is_complete("context_episode_anchors", "db")?);
    assert!(db.storage_domain_is_complete("turn_records", "db")?);
    assert!(db.storage_domain_is_complete("evidence", "db")?);
    Ok(())
}

#[test]
fn original_work_footprint_failure_has_original_failed_marker_and_retries_same_sealed_source(
) -> Result<()> {
    for malformed in [true, false] {
        let (_home, storage) = fixture()?;
        let db = &storage.runtime_db;
        seed_parity(&storage)?;
        let source = source(&storage);
        db.freeze_original_episode_import(&source)?;
        let original: String = db.connection()?.query_row(
            "SELECT payload_json FROM briefs WHERE evidence_id='z'",
            [],
            |r| r.get(0),
        )?;
        // The original raw gate precedes native JSON parsing, which precedes
        // the sealed digest check. Exercise both failure boundaries explicitly.
        let changed = if malformed {
            "{".to_owned()
        } else {
            format!("{original}\n")
        };
        db.transaction(|tx| {
            tx.execute(
                "UPDATE briefs SET payload_json=?1 WHERE evidence_id='z'",
                [&changed],
            )?;
            Ok(())
        })?;
        let error = db.import_original_episode_evidence(&source).unwrap_err();
        let expected = if malformed {
            "malformed JSON"
        } else {
            "original episode source version changed"
        };
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        let (status,temp):(String,u64)=db.connection()?.query_row("SELECT import_status,(SELECT COUNT(*) FROM sqlite_temp_master WHERE name LIKE 'original_work_import_%') FROM storage_domains WHERE domain='evidence'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!((status.as_str(), temp), ("failed", 0));
        assert!(!db.storage_domain_is_complete("evidence", "db")?);
        db.transaction(|tx| {
            tx.execute(
                "UPDATE briefs SET payload_json=?1 WHERE evidence_id='z'",
                [&original],
            )?;
            insert_brief_evidence_tx(tx, &brief("late-raw"))?;
            Ok(())
        })?;
        db.import_original_episode_evidence(&source)?;
        let late:bool=db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM storage_domains d,json_each(d.source_checkpoint_json,'$.work_domain_actual_writes.records') j WHERE d.domain='evidence' AND j.key='brief:late-raw')",[],|r|r.get(0))?;
        assert!(!late);
    }
    Ok(())
}
