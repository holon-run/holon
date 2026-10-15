use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        evidence::{insert_delivery_summary_evidence_tx, upsert_agent_identity_tx},
        input_evidence_snapshot::InputEvidenceSnapshotError,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        AuthorityClass, BriefAttachment, BriefKind, BriefRecord, DeliverySummaryRecord,
        ToolExecutionRecord, ToolExecutionStatus, TranscriptEntry, TranscriptEntryKind,
    },
};
use std::sync::Arc;

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
fn summary(id: &str) -> DeliverySummaryRecord {
    let mut record =
        DeliverySummaryRecord::new("original", "work-original", "actual summary", Some(3), None);
    record.id = id.into();
    record.turn_id = Some("original-turn".into());
    record.created_at = "2026-10-14T01:00:00.000000123Z".parse().unwrap();
    record
}
fn source(storage: &AppStorage) -> OriginalEpisodeImport<'_> {
    OriginalEpisodeImport {
        agent_id: storage.agent_id.as_deref().unwrap(),
    }
}

#[test]
fn original_storage_evidence_streams_100001_received_sql_rows_without_a_delivery_vec() -> Result<()>
{
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    // This is the actual Evidence phase/caller. The separate Turn aggregation
    // phase is deliberately already complete; its large-ID closure is separate.
    db.turn_records()
        .import_legacy(Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())?;
    db.transaction(|tx| {
        for index in 0..100_001 {
            insert_delivery_summary_evidence_tx(tx, &summary(&format!("delivery-{index:06}")))?;
        }
        Ok(())
    })?;
    storage.legacy_importer().import_episode_ref_domains()?;
    let connection = db.connection()?;
    let (count,sealed):(u64,bool)=connection.query_row("SELECT member_count,sealed FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/actual_evidence_import'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!((count, sealed), (100_001, true));
    let frozen: u64 = connection.query_row(
        "SELECT COUNT(*) FROM legacy_episode_import_members WHERE family='delivery'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(frozen, 100_001);
    assert!(db.storage_domain_is_complete("evidence", "db")?);
    let aliases: u64 = connection.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE family='legacy_delivery'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        aliases, 0,
        "original import remains birth0 and creates no runtime alias"
    );
    drop(connection);
    // Complete-domain replay cannot adopt a new raw row or inspect its body.
    db.evidence()
        .append_delivery_summary(&summary("late-raw"))?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE delivery_summaries SET payload_json='{' WHERE evidence_id='late-raw'",
            [],
        )?;
        Ok(())
    })?;
    storage.legacy_importer().import_episode_ref_domains()?;
    let late:bool=db.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM input_legacy_delivery_fixed_members WHERE capture_id='legacy_delivery/v1/actual_evidence_import' AND record_id='late-raw')",[],|r|r.get(0))?;
    assert!(!late);
    Ok(())
}

#[test]
fn original_storage_stream_uses_one_actual_scratch_until_the_complete_receipt() -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    db.transaction(|tx| {
        for index in 0..100_001 {
            insert_delivery_summary_evidence_tx(tx, &summary(&format!("delivery-{index:06}")))?;
        }
        Ok(())
    })?;
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    let peak = db.import_original_episode_evidence_with_scratch_for_test(&source, &scratch)?;
    assert!(
        peak > 65_536 && peak < 1024 * 1024,
        "owned Rust scratch estimate={peak}, not RSS"
    );
    assert_eq!(scratch.report()?.observed_bytes, 0);
    assert!(db.storage_domain_is_complete("evidence", "db")?);
    let count:u64=db.connection()?.query_row("SELECT member_count FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/actual_evidence_import'",[],|r|r.get(0))?;
    assert_eq!(count, 100_001);
    Ok(())
}

#[test]
fn received_evidence_adapter_accounts_all_four_still_owned_vectors_before_new_writes() -> Result<()>
{
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    let nodes = || serde_json::Value::Array(vec![serde_json::Value::Null; 2100]);
    let transcript = TranscriptEntry::new(
        "original",
        TranscriptEntryKind::AssistantRound,
        Some(1),
        None,
        nodes(),
    );
    let tool = ToolExecutionRecord {
        id: "tool-original".into(),
        agent_id: "original".into(),
        work_item_id: None,
        turn_index: 0,
        turn_id: None,
        tool_name: "original".into(),
        created_at: chrono::Utc::now(),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: nodes(),
        output: serde_json::Value::Null,
        summary: "original".into(),
        invocation_surface: None,
    };
    let mut brief = BriefRecord::new("original", BriefKind::Ack, "original", None, None);
    brief.attachments = Some(vec![BriefAttachment {
        kind: "original".into(),
        name: "original".into(),
        uri: None,
        value: Some(nodes()),
    }]);
    let mut delivery = summary("delivery-original");
    delivery.evidence = Some(nodes());
    for cost in [
        RuntimeDb::original_received_material_for_test(&transcript)?,
        RuntimeDb::original_received_material_for_test(&tool)?,
        RuntimeDb::original_received_material_for_test(&brief)?,
        RuntimeDb::original_received_material_for_test(&delivery)?,
    ] {
        assert!(cost.1 < 65_536 && cost.0 < 64 * 1024 * 1024);
    }
    let error = db
        .evidence()
        .import_legacy(
            Vec::new(),
            vec![transcript],
            vec![tool],
            vec![brief],
            vec![delivery],
        )
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(
                "original Evidence import received source"
            ))
        ),
        "{error:#}"
    );
    let connection = db.connection()?;
    let count:u64=connection.query_row("SELECT (SELECT COUNT(*) FROM transcript_entries)+(SELECT COUNT(*) FROM tool_executions)+(SELECT COUNT(*) FROM briefs)+(SELECT COUNT(*) FROM delivery_summaries)",[],|r|r.get(0))?;
    assert_eq!(count, 0);
    assert!(!db.storage_domain_is_complete("evidence", "db")?);
    let source:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/actual_evidence_import')",[],|r|r.get(0))?;
    assert!(!source);
    Ok(())
}

#[test]
fn frozen_original_episode_cohort_survives_failed_import_and_never_recaptures_late_raw(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    let original = summary("original");
    db.evidence().append_delivery_summary(&original)?;
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    let header: String = db.connection()?.query_row(
        "SELECT metadata_digest FROM legacy_episode_import_captures",
        [],
        |r| r.get(0),
    )?;
    db.evidence()
        .append_delivery_summary(&summary("late-raw"))?;
    let mut changed = original.clone();
    changed.text = "different actual source version".into();
    db.evidence().append_delivery_summary(&changed)?;
    let error = db.import_original_episode_evidence(&source).unwrap_err();
    assert!(
        format!("{error:#}").contains("original episode source version changed"),
        "{error:#}"
    );
    let connection = db.connection()?;
    let (status,members,partial):(String,u64,bool)=connection.query_row("SELECT (SELECT import_status FROM storage_domains WHERE domain='evidence'),(SELECT member_count FROM legacy_episode_import_captures),EXISTS(SELECT 1 FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/actual_evidence_import')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!((status, members, partial), ("failed".into(), 1, false));
    drop(connection);
    db.evidence().append_delivery_summary(&original)?;
    db.freeze_original_episode_import(&source)?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    db.import_original_episode_evidence_with_scratch_for_test(&source, &scratch)?;
    assert_eq!(scratch.report()?.observed_bytes, 0);
    let connection = db.connection()?;
    let (same,late,count):(bool,bool,u64)=connection.query_row("SELECT (SELECT metadata_digest=?1 FROM legacy_episode_import_captures),EXISTS(SELECT 1 FROM input_legacy_delivery_fixed_members WHERE capture_id='legacy_delivery/v1/actual_evidence_import' AND record_id='late-raw'),(SELECT member_count FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/actual_evidence_import')",[header],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!((same, late, count), (true, false, 1));
    Ok(())
}

#[test]
fn original_episode_freeze_rejects_bad_second_row_atomically_and_foreign_or_successor_scope(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = &storage.runtime_db;
    db.evidence().append_delivery_summary(&summary("a-good"))?;
    let bad = summary("z-bad");
    db.evidence().append_delivery_summary(&bad)?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE delivery_summaries SET payload_json=?1 WHERE evidence_id='z-bad'",
            ["{".repeat(9 * 1024 * 1024)],
        )?;
        Ok(())
    })?;
    let original = source(&storage);
    let error = db.freeze_original_episode_import(&original).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(
                "original episode source scratch"
            ))
        ),
        "{error:#}"
    );
    let count: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM legacy_episode_import_captures",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    db.evidence().append_delivery_summary(&bad)?;
    db.freeze_original_episode_import(&original)?;
    let foreign = OriginalEpisodeImport {
        agent_id: "foreign",
    };
    assert!(db
        .freeze_original_episode_import(&foreign)
        .unwrap_err()
        .to_string()
        .contains("foreign"));
    // Adversarial identity replacement; never a public Host producer fixture.
    db.transaction(|tx|{tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id='original'",[])?;Ok(())})?;
    assert!(db
        .freeze_original_episode_import(&original)
        .unwrap_err()
        .to_string()
        .contains("incarnation changed"));
    assert!(!db.storage_domain_is_complete("evidence", "db")?);
    Ok(())
}

#[test]
fn original_v81_unbound_source_keeps_its_exact_seed_and_cannot_be_claimed_by_fresh_identity(
) -> Result<()> {
    use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
    let home = tempfile::tempdir()?;
    let path = home.path().join("old.sqlite");
    {
        let mut connection = rusqlite::Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
            apply_migration(&mut connection, migration)?;
        }
        let tx = connection.transaction()?;
        crate::runtime_db::evidence::upsert_agent_state_tx(
            &tx,
            &crate::types::AgentState::new("original"),
        )?;
        insert_delivery_summary_evidence_tx(&tx, &summary("old-original"))?;
        tx.commit()?;
    }
    let db = RuntimeDb::open_and_migrate(path, home.path().join("old.lock"))?;
    let storage = AppStorage::new_for_agent(home.path(), "original", db.clone())?;
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    let connection = db.connection()?;
    let (same_seed,inc):(bool,Option<u64>)=connection.query_row("SELECT EXISTS(SELECT 1 FROM legacy_episode_import_captures c JOIN episode_continuity_seeds s ON s.seed_id=c.seed_id WHERE s.agent_id='original' AND s.source_kind='upgrade_unbound' AND s.identity_incarnation IS NULL),identity_incarnation FROM legacy_episode_import_captures",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert!(same_seed);
    assert_eq!(inc, None);
    drop(connection);
    // Explicit adversarial scope change: a later identity does not replace the
    // old unbound cohort proof merely because its agent ID is equal.
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
    let error = db.import_original_episode_evidence(&source).unwrap_err();
    assert!(
        format!("{error:#}").contains("original episode source incarnation changed"),
        "{error:#}"
    );
    let source_count: u64 = db.connection()?.query_row(
        "SELECT member_count FROM legacy_episode_import_captures",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(source_count, 1);
    assert!(!db.storage_domain_is_complete("evidence", "db")?);
    Ok(())
}

#[tokio::test]
async fn fresh_standalone_runtime_imports_empty_episode_before_identity_materialization(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let runtime = crate::runtime::RuntimeHandle::new(
        "fresh-original",
        home.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(crate::provider::StubProvider::new("unused")),
        "fresh-original".into(),
        crate::context::ContextConfig::default(),
    )?;
    let db = runtime.runtime_db();
    let (empty,inc,seed,count,sealed):(bool,Option<u64>,Option<String>,u64,bool)=db.connection()?.query_row(
        "SELECT empty_original,identity_incarnation,seed_id,member_count,sealed FROM legacy_episode_import_captures WHERE capture_id='original_episode_refs/v1' AND agent_id='fresh-original'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    assert_eq!(
        (empty, inc, seed, count, sealed),
        (true, None, None, 0, true)
    );
    let identity = db
        .agent_identities()
        .latest("fresh-original")?
        .expect("actual constructor materializes identity after the empty import");
    assert!(identity.incarnation > 0);
    for domain in ["turn_records", "evidence", "context_episode_anchors"] {
        assert!(db.storage_domain_is_complete(domain, "db")?, "{domain}");
    }
    assert_eq!(runtime.agent_state().await?.id, "fresh-original");
    drop(runtime);
    Ok(())
}

#[test]
fn original_empty_freeze_survives_identity_materialization_and_never_adopts_raw_tails() -> Result<()>
{
    let home = tempfile::tempdir()?;
    let storage = AppStorage::new_for_test_with_agent(home.path(), "original")?;
    let db = &storage.runtime_db;
    assert!(db.agent_identities().latest("original")?.is_none());
    let source = source(&storage);
    db.freeze_original_episode_import(&source)?;
    assert!(db.original_episode_import_is_empty(&source)?);
    let before: String = db.connection()?.query_row(
        "SELECT metadata_digest FROM legacy_episode_import_captures",
        [],
        |r| r.get(0),
    )?;
    // Simulate process exit between source freeze and the two business imports.
    db.transaction(|tx| {
        tx.execute("INSERT INTO delivery_summaries(evidence_id,agent_id,created_at,kind,payload_json) VALUES('late-delivery','original','t','delivery_summary','{')",[])?;
        tx.execute("INSERT INTO wait_conditions(wait_condition_id,agent_id,status,kind,waiting_for,created_at,updated_at,last_turn_id,payload_json) VALUES('late-wait','original','active','external','late','t','t','late-turn','{')",[])?;
        upsert_agent_identity_tx(tx,&AgentIdentityRecord::new("original",AgentKind::Named,AgentVisibility::Public,AgentOwnership::SelfOwned,AgentProfilePreset::PublicNamed,None,None))?;
        Ok(())
    })?;
    db.freeze_original_episode_import(&source)?;
    storage.legacy_importer().import_episode_ref_domains()?;
    let (same,members,turns,delivery_members,raw_delivery,raw_wait):(bool,u64,u64,u64,bool,bool)=db.connection()?.query_row(
        "SELECT (SELECT metadata_digest=?1 AND empty_original=1 AND member_count=0 AND identity_incarnation IS NULL AND seed_id IS NULL FROM legacy_episode_import_captures),(SELECT COUNT(*) FROM legacy_episode_import_members),(SELECT COUNT(*) FROM turn_records),(SELECT member_count FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/actual_evidence_import'),EXISTS(SELECT 1 FROM delivery_summaries WHERE evidence_id='late-delivery'),EXISTS(SELECT 1 FROM wait_conditions WHERE wait_condition_id='late-wait')",
        [before],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    assert_eq!(
        (
            same,
            members,
            turns,
            delivery_members,
            raw_delivery,
            raw_wait
        ),
        (true, 0, 0, 0, true, true)
    );
    assert!(db.storage_domain_is_complete("turn_records", "db")?);
    assert!(db.storage_domain_is_complete("evidence", "db")?);
    Ok(())
}

#[test]
fn nonempty_unbound_episode_or_wait_cannot_claim_original_empty_source() -> Result<()> {
    for table in [
        "messages",
        "transcript_entries",
        "tool_executions",
        "briefs",
        "delivery_summaries",
        "wait_conditions",
    ] {
        let home = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test_with_agent(home.path(), "original")?;
        let db = &storage.runtime_db;
        db.transaction(|tx| {
            if table=="wait_conditions" {
                tx.execute("INSERT INTO wait_conditions(wait_condition_id,agent_id,status,kind,waiting_for,created_at,updated_at,payload_json) VALUES('original-row','original','active','external','pending','t','t','{')",[])?;
            } else {
                tx.execute(&format!("INSERT INTO {table}(evidence_id,agent_id,created_at,kind,payload_json) VALUES('original-row','original','t','legacy','{{')"),[])?;
            }
            Ok(())
        })?;
        let error = db
            .freeze_original_episode_import(&source(&storage))
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("no original unbound seed"),
            "{table}: {error:#}"
        );
        let count: u64 = db.connection()?.query_row(
            "SELECT COUNT(*) FROM legacy_episode_import_captures",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 0, "{table}");
        assert!(!db.storage_domain_is_complete("evidence", "db")?);
        assert!(!db.storage_domain_is_complete("turn_records", "db")?);
    }
    Ok(())
}
