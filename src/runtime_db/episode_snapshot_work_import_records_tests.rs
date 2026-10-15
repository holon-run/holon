use super::*;
use crate::{
    runtime_db::{evidence::upsert_agent_identity_tx, ExpectedStorageDomain, RuntimeDb},
    storage::AppStorage,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
        BriefKind, BriefRecord,
    },
};

fn fixture() -> Result<(tempfile::TempDir, AppStorage)> {
    let home = tempfile::tempdir()?;
    let storage = AppStorage::new_for_test_with_agent(home.path(), "original")?;
    storage
        .runtime_db()?
        .context("actual fixture RuntimeDb missing")?
        .transaction(|tx| {
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
fn brief(id: &str, text: &str) -> BriefRecord {
    let mut record = BriefRecord::new("original", BriefKind::Result, text, None, None);
    record.id = id.into();
    record.work_item_id = Some("original-work".into());
    record.created_at = "2026-10-14T01:00:00.000000123Z".parse().unwrap();
    record
}
fn received(db: &RuntimeDb, records: Vec<BriefRecord>) -> Result<()> {
    db.evidence()
        .import_legacy(Vec::new(), Vec::new(), Vec::new(), records, Vec::new())
}

#[test]
fn work_import_reader_keeps_full_nul_keys_exact_digest_scope_and_actual_connection() -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = storage
        .runtime_db()?
        .context("actual fixture RuntimeDb missing")?;
    let record = brief("é\u{0}tail\n", "original");
    received(&db, vec![record.clone()])?;
    db.transaction_once(|tx| {
        let view = ImportedRecords::load(tx, "original")?;
        let scratch = view.scratch.clone();
        let payload: String = tx.query_row(
            "SELECT payload_json FROM briefs WHERE evidence_id=?1",
            [&record.id],
            |r| r.get(0),
        )?;
        let digest = bytes_digest(payload.as_bytes());
        assert!(view.matches(tx, "brief:é\u{0}tail\n", &digest)?);
        assert!(!view.has(tx, "brief:é")?);
        assert!(!view.matches(tx, "brief:é\u{0}tail\n", &"b".repeat(64))?);
        let other = Connection::open_in_memory()?;
        let error = view.has(&other, "brief:é\u{0}tail\n").unwrap_err();
        assert!(format!("{error:#}").contains("actual connection"));
        assert!(view.peak.load(std::sync::atomic::Ordering::Relaxed) < 1024 * 1024);
        view.finish(tx)?;
        assert_eq!(scratch.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn work_import_reader_domain_precedence_foreign_skip_and_private_view_replacement_are_explicit(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = storage
        .runtime_db()?
        .context("actual fixture RuntimeDb missing")?;
    received(&db, Vec::new())?;
    db.transaction_once(|tx|{
        // Adversarial persisted v1 metadata tests the reader's exact closed
        // record shapes; it grants no ordinary alias or runtime Ready state.
        let entry=|agent:&str,digest:&str|serde_json::json!({"agent_id":agent,
            "raw_digest":digest,"origin":"actual_write","work_item_id":null});
        let checkpoint=|record:serde_json::Value|serde_json::json!({"work_domain_actual_writes":{
            "policy_version":1,"records":{"message:original":record}}});
        let first=serde_json::to_string(&checkpoint(entry("original","first")))?;
        let later=serde_json::to_string(&checkpoint(entry("foreign","foreign")))?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,source_checkpoint_json,updated_at) VALUES('messages',1,'complete','db',?1,'t')",[first])?;
        tx.execute("UPDATE storage_domains SET source_checkpoint_json=?1 WHERE domain='evidence'",[later])?;
        let view=ImportedRecords::load(tx,"original")?;
        assert!(view.matches(tx,"message:original","first")?);
        let mut seen=Vec::new();
        view.visit_messages(tx,|id,digest|{seen.push((id.to_owned(),digest.to_owned()));Ok(())})?;
        assert_eq!(seen,vec![("original".into(),"first".into())]);
        let successor=ImportedRecords::load(tx,"original")?;
        let error=view.has(tx,"message:original").unwrap_err();
        assert!(format!("{error:#}").contains("view was replaced"));
        drop(view);
        successor.finish(tx)?;
        Ok(())
    })
}

#[test]
fn work_import_reader_counts_full_native_record_before_any_record_decoder_and_rolls_back_scratch(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = storage
        .runtime_db()?
        .context("actual fixture RuntimeDb missing")?;
    received(&db, vec![brief("original", "original")])?;
    db.transaction(|tx| {
        let wide = serde_json::json!({"work_domain_actual_writes":{"policy_version":1,
            "records":{"brief:original":{"agent_id":"original","raw_digest":"a",
                "origin":"actual_write","work_item_id":null,
                "unknown_padding":vec![serde_json::Value::Null;9000]}}}});
        tx.execute(
            "UPDATE storage_domains SET source_checkpoint_json=?1 WHERE domain='evidence'",
            [serde_json::to_string(&wide)?],
        )?;
        Ok(())
    })?;
    let result: Result<()> = db.transaction_once(|tx| {
        let _view = ImportedRecords::load(tx, "original")?;
        Ok(())
    });
    let error = result.unwrap_err();
    assert_eq!(
        error.downcast_ref::<EpisodeSnapshotError>(),
        Some(&EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "legacy Work import row material"
        })
    );
    let temp: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM sqlite_temp_master WHERE name LIKE 'original_work_import_read_%'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(temp, 0);
    assert!(db.storage_domain_is_complete("evidence", "db")?);
    Ok(())
}

#[test]
fn work_import_head_uses_original_accepted_nonempty_order_without_inspecting_late_body_and_keeps_guards(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = storage
        .runtime_db()?
        .context("actual fixture RuntimeDb missing")?;
    received(
        &db,
        vec![
            brief("accepted", "original"),
            brief("empty", ""),
            brief("nul", "\u{0}later"),
        ],
    )?;
    db.evidence().append_brief(&brief("zz-late", "raw late"))?;
    db.transaction_once(|tx| {
        tx.execute(
            "UPDATE briefs SET payload_json='{' WHERE evidence_id='zz-late'",
            [],
        )?;
        let view = ImportedRecords::load(tx, "original")?;
        let scratch = view.scratch.clone();
        let (head, guards) = view.select_head(tx, "original", "original-work", &BTreeSet::new())?;
        let weak = guards.iter().map(Arc::downgrade).collect::<Vec<_>>();
        let (id, payload) = head.context("original nonempty head missing")?;
        assert_eq!(id, "accepted");
        let record: BriefRecord = serde_json::from_str(&payload)?;
        assert_eq!(record.text, "original");
        assert!(weak.iter().all(|guard| guard.upgrade().is_some()));
        assert!(!view.has(tx, "brief:zz-late")?);
        drop(record);
        drop(payload);
        drop(id);
        drop(guards);
        assert!(weak.iter().all(|guard| guard.upgrade().is_none()));
        view.finish(tx)?;
        assert_eq!(scratch.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn storage_complete_marker_and_success_diagnostics_do_not_require_the_checkpoint_body_decoder(
) -> Result<()> {
    let (_home, storage) = fixture()?;
    let db = storage
        .runtime_db()?
        .context("actual fixture RuntimeDb missing")?;
    received(&db, Vec::new())?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE storage_domains SET source_checkpoint_json='{' WHERE domain='evidence'",
            [],
        )?;
        Ok(())
    })?;
    let expected = [ExpectedStorageDomain {
        domain: "evidence",
        canonical_source: "db",
    }];
    assert!(db.storage_domain_is_complete("evidence", "db")?);
    assert!(db.diagnose_cutover(&expected)?.is_empty());
    db.transaction(|tx|{
        tx.execute("UPDATE storage_domains SET import_status='failed',canonical_source='jsonl',source_checkpoint_json='original failure' WHERE domain='evidence'",[])?;
        Ok(())
    })?;
    assert!(!db.storage_domain_is_complete("evidence", "db")?);
    assert_eq!(
        db.diagnose_cutover(&expected)?,
        vec![
            "storage domain evidence import failed: original failure".to_owned(),
            "storage domain evidence has canonical_source=jsonl; expected db".to_owned(),
        ]
    );
    Ok(())
}
