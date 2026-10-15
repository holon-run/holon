use super::*;
use crate::{
    runtime_db::{evidence::upsert_agent_identity_tx, RuntimeDb},
    types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};

fn db() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
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
    Ok((home, db))
}
fn summary(id: &str, text: &str) -> DeliverySummaryRecord {
    let mut record = DeliverySummaryRecord::new("original", "work-original", text, Some(3), None);
    record.id = id.into();
    record.turn_id = Some("original-turn".into());
    record.created_at = "2026-10-14T01:00:00.000000123Z".parse().unwrap();
    record
}
fn header(scratch: &InputEvidenceBudgetLedger) -> Result<Arc<BudgetViewGuard>> {
    Ok(scratch.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: 4096,
        },
    )?)
}

#[test]
fn original_evidence_import_seals_only_actual_delivery_writes_and_final_duplicate_version(
) -> Result<()> {
    let (_home, db) = db()?;
    db.evidence()
        .append_delivery_summary(&summary("unreceived-raw", "not an original import member"))?;
    let old = summary("repeated", "old");
    let mut final_record = summary("repeated", "");
    final_record.created_at += chrono::Duration::milliseconds(1);
    db.evidence().import_legacy(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![old, final_record.clone(), summary("received", "text")],
    )?;
    let connection = db.connection()?;
    let (members, sealed, exact_inc): (u64, bool, bool) = connection.query_row(
        "SELECT member_count,sealed,NOT EXISTS(SELECT 1 FROM input_legacy_delivery_fixed_members WHERE capture_id=?1 AND (identity_incarnation!=1 OR seed_id IS NOT NULL)) FROM input_legacy_delivery_captures WHERE capture_id=?1",
        [IMPORT], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!((members, sealed, exact_inc), (2, true, true));
    let (digest, nonempty): (String,bool) = connection.query_row(
        "SELECT content_digest,text_nonempty FROM input_legacy_delivery_fixed_members WHERE capture_id=?1 AND record_id='repeated'",
        [IMPORT], |r| Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!(
        digest,
        bytes_digest(serde_json::to_string(&final_record)?.as_bytes())
    );
    assert!(!nonempty);
    let raw: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM input_legacy_delivery_fixed_members WHERE capture_id=?1 AND record_id='unreceived-raw')",[IMPORT],|r|r.get(0))?;
    assert!(!raw);
    let checkpoint: String = connection.query_row("SELECT source_checkpoint_json FROM storage_domains WHERE domain='evidence' AND import_status='complete' AND canonical_source='db'",[],|r|r.get(0))?;
    let checkpoint: serde_json::Value = serde_json::from_str(&checkpoint)?;
    assert_eq!(checkpoint["legacy_delivery_source"][0], 2);
    assert_eq!(
        checkpoint["legacy_delivery_source"][1]
            .as_str()
            .map(str::len),
        Some(64)
    );
    assert_eq!(
        checkpoint["work_domain_actual_writes"]["records"],
        serde_json::json!({})
    );
    let aliases: u64 = connection.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE family='legacy_delivery'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        aliases, 0,
        "import is a fixed source, not a native alias or birth"
    );
    drop(connection);
    // Complete-domain replay returns before any new received/raw tail adoption.
    let mut rejected = summary("late-raw", "unused");
    rejected.evidence = Some(serde_json::Value::Array(vec![
        serde_json::Value::Null;
        9000
    ]));
    db.evidence().import_legacy(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![rejected],
    )?;
    let late: bool = db.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM delivery_summaries WHERE evidence_id='late-raw')",
        [],
        |r| r.get(0),
    )?;
    assert!(!late);
    Ok(())
}

#[test]
fn original_evidence_bad_delivery_rolls_back_every_member_write_and_complete_receipt() -> Result<()>
{
    let (_home, db) = db()?;
    let mut bad = summary("z-bad", "bad");
    bad.evidence = Some(serde_json::Value::Array(vec![
        serde_json::Value::Null;
        9000
    ]));
    let error = db
        .evidence()
        .import_legacy(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![summary("a-good", "good"), bad],
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
    let (canonical, captures, members, complete): (u64,u64,u64,bool) = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM delivery_summaries),(SELECT COUNT(*) FROM input_legacy_delivery_captures WHERE capture_id=?1),(SELECT COUNT(*) FROM input_legacy_delivery_fixed_members WHERE capture_id=?1),EXISTS(SELECT 1 FROM storage_domains WHERE domain='evidence' AND import_status='complete')",
        [IMPORT], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    assert_eq!((canonical, captures, members, complete), (0, 0, 0, false));
    drop(connection);
    db.evidence().import_legacy(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![summary("a-good", "good"), summary("z-bad", "repaired")],
    )?;
    let (members, sealed): (u64, bool) = db.connection()?.query_row(
        "SELECT member_count,sealed FROM input_legacy_delivery_captures WHERE capture_id=?1",
        [IMPORT],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!((members, sealed), (2, true));
    Ok(())
}

#[test]
fn original_delivery_import_streams_more_than_record_cap_with_one_shared_scratch() -> Result<()> {
    let (_home, db) = db()?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut peak = 0u64;
    db.transaction(|tx| {
        let _header = header(&scratch)?;
        let original = LegacyDeliveryImport::begin_if_installed(tx, &scratch, 0)?
            .context("actual installed schema absent")?;
        for index in 0..100_001 {
            original.append_original(&summary(&format!("received-{index:06}"), "original"))?;
            // The current row has dropped; the original header is the only
            // retained actor object. No complete ID collection exists in Rust.
            peak = peak.max(scratch.report()?.observed_bytes);
            assert_eq!(scratch.report()?.observed_bytes, 4096);
        }
        assert!(original.peak_owned_bytes_for_test() > 4096);
        assert!(
            original.peak_owned_bytes_for_test() < 512 * 1024,
            "actual live row owned peak: {}",
            original.peak_owned_bytes_for_test()
        );
        let (members, digest) = original.finish()?;
        assert_eq!(members, 100_001);
        assert_eq!(digest.len(), 64);
        Ok(())
    })?;
    assert_eq!(peak, 4096);
    assert_eq!(scratch.report()?.observed_bytes, 0);
    let (members, sealed): (u64, bool) = db.connection()?.query_row(
        "SELECT member_count,sealed FROM input_legacy_delivery_captures WHERE capture_id=?1",
        [IMPORT],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!((members, sealed), (100_001, true));
    Ok(())
}

#[test]
fn original_delivery_import_combines_existing_work_map_and_row_native_before_decode() -> Result<()>
{
    let (_home, db) = db()?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut writes =
        crate::runtime_db::episode_snapshot::work_checkpoint::imports::ImportWrites::default();
    for index in 0..400 {
        writes.record("message",&format!("actual-message-{index}"),"original",crate::runtime_db::episode_snapshot::work_checkpoint::imports::ImportOrigin::ActualWrite)?;
    }
    let prior = writes.input_import_material_estimate()?;
    assert!(prior.0 < MAX_METADATA_BYTES as u64 && prior.1 < MAX_RECORDS);
    let mut record = summary("joint-row", "actual");
    record.evidence = Some(serde_json::Value::Array(vec![
        serde_json::Value::Null;
        2500
    ]));
    let error = db
        .transaction(|tx| {
            let _header = header(&scratch)?;
            let _prior = observe_existing_import_metadata(&scratch, prior)?;
            let original = LegacyDeliveryImport::begin_if_installed(tx, &scratch, prior.1)?
                .context("installed capture absent")?;
            original.append_original(&record)?;
            original.finish()?;
            Ok(())
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(
                "LegacyDelivery fixed native"
            ))
        ),
        "{error:#}"
    );
    assert_eq!(scratch.report()?.observed_bytes, 0);
    let count: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM delivery_summaries WHERE evidence_id='joint-row'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    // Exactly the same source fits without the simultaneously owned Work map.
    db.transaction(|tx| {
        let _header = header(&scratch)?;
        let original = LegacyDeliveryImport::begin_if_installed(tx, &scratch, 0)?
            .context("installed capture absent")?;
        original.append_original(&record)?;
        assert_eq!(original.finish()?.0, 1);
        Ok(())
    })?;
    assert_eq!(scratch.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn installed_legacy_delivery_import_missing_schema_is_not_silently_skipped() -> Result<()> {
    let (_home, db) = db()?;
    db.transaction(|tx| {
        tx.execute_batch("DROP TABLE input_legacy_delivery_fixed_members; DROP TABLE input_legacy_delivery_captures")?;
        Ok(())
    })?;
    let error = db
        .evidence()
        .import_legacy(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![summary("received", "actual")],
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("installed import schema missing"),
        "{error:#}"
    );
    let count: u64 =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM delivery_summaries", [], |r| r.get(0))?;
    assert_eq!(count, 0);
    assert!(!db.storage_domain_is_complete("evidence", "db")?);
    Ok(())
}
