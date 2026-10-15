use super::*;
use crate::{
    runtime_db::{
        evidence::{insert_delivery_summary_evidence_tx, upsert_agent_identity_tx},
        input_evidence_snapshot::{read_material::InputReadMaterial, InputEvidenceLoadRequest},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        RuntimeDb,
    },
    types::{AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use rusqlite::Connection;
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "original".into(),
        identity_incarnation: 1,
    }
}
fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        "original",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
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
fn summary(id: &str, text: &str, millis: i64) -> DeliverySummaryRecord {
    let mut record = DeliverySummaryRecord::new("original", "original-work", text, Some(3), None);
    record.id = id.into();
    record.created_at = "2026-10-14T01:00:00.000000123Z".parse().unwrap();
    record.created_at += chrono::Duration::milliseconds(millis);
    record
}
fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db =
        RuntimeDb::open_and_migrate(home.path().join("db.sqlite"), home.path().join("db.lock"))?;
    db.transaction(|tx| upsert_agent_identity_tx(tx, &identity()))?;
    Ok((home, db))
}
fn imported(db: &RuntimeDb, records: Vec<DeliverySummaryRecord>) -> Result<()> {
    db.evidence()
        .import_legacy(Vec::new(), Vec::new(), Vec::new(), Vec::new(), records)
}
fn observed_id(value: &LegacyDeliveryHeadObservation) -> Option<&str> {
    match value {
        LegacyDeliveryHeadObservation::Present { source } => Some(&source.key.id),
        _ => None,
    }
}
#[test]
fn accepted_delivery_selectors_preserve_latest_any_vs_nonempty_and_actual_material_until_wire(
) -> Result<()> {
    let (_home, db) = fixture()?;
    imported(
        &db,
        vec![
            summary("older", "accepted", 0),
            summary("newest-empty", "", 1),
        ],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let original = scope();
    let (any, nonempty, guards) = db.transaction_once(|tx| {
        initialize_scope_tx(tx, &original, None)?;
        let mut material = InputReadMaterial::new(&original, &request(), &ledger, (0, 0))?;
        let any = observe_head_tx(
            tx,
            &original,
            "original-work",
            LegacyDeliveryHeadSelector::LatestAny,
            |b, r| material.observe_projected(b, r),
        )?;
        let nonempty = observe_head_tx(
            tx,
            &original,
            "original-work",
            LegacyDeliveryHeadSelector::LatestNonempty,
            |b, r| material.observe_projected(b, r),
        )?;
        let absent = observe_head_tx(
            tx,
            &original,
            "absent-work",
            LegacyDeliveryHeadSelector::LatestAny,
            |b, r| material.observe_projected(b, r),
        )?;
        assert_eq!(absent, LegacyDeliveryHeadObservation::Absent);
        Ok((any, nonempty, material.into_guards()))
    })?;
    assert_eq!(observed_id(&any), Some("newest-empty"));
    assert_eq!(observed_id(&nonempty), Some("older"));
    for observation in [&any, &nonempty] {
        match observation {
            LegacyDeliveryHeadObservation::Present { source } => {
                assert_eq!(source.scope, original);
                assert_eq!(source.source_revision, 0);
                assert_eq!(
                    source.availability,
                    LegacyDeliverySourceAvailability::Present
                );
            }
            _ => unreachable!(),
        }
    }
    let weak = guards.iter().map(Arc::downgrade).collect::<Vec<_>>();
    let wire = serde_json::to_string(&(&any, &nonempty))?;
    assert!(wire.contains("newest-empty") && wire.contains("older"));
    assert!(ledger.report()?.observed_bytes > 0);
    drop(wire);
    drop(any);
    drop(nonempty);
    drop(guards);
    assert!(weak.iter().all(|guard| guard.upgrade().is_none()));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn accepted_delivery_scope_initializes_once_ignores_raw_late_and_pruned_head_does_not_jump_back(
) -> Result<()> {
    let (_home, db) = fixture()?;
    imported(
        &db,
        vec![
            summary("older", "accepted", 0),
            summary("newest", "accepted", 1),
        ],
    )?;
    db.transaction(|tx| initialize_scope_tx(tx, &scope(), None))?;
    db.evidence()
        .append_delivery_summary(&summary("raw-late", "raw", 2))?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE delivery_summaries SET payload_json='{' WHERE evidence_id='raw-late'",
            [],
        )?;
        initialize_scope_tx(tx, &scope(), None)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction_once(|tx|{
        let original=scope();
        let mut material=InputReadMaterial::new(&original,&request(),&ledger,(0,0))?;
        let before=observe_head_tx(tx,&original,"original-work",LegacyDeliveryHeadSelector::LatestNonempty,|b,r|material.observe_projected(b,r))?;
        assert_eq!(observed_id(&before),Some("newest"));
        // Component proof: the original qualified removal has deleted this
        // exact body; the Source transfers its fixed fingerprint to a tombstone.
        tx.execute("DELETE FROM delivery_summaries WHERE evidence_id='newest'",[])?;
        assert!(prune_fixed_tx(tx,&original,&Key::new(Family::LegacyDelivery,"newest"))?.is_some());
        let after=observe_head_tx(tx,&original,"original-work",LegacyDeliveryHeadSelector::LatestNonempty,|b,r|material.observe_projected(b,r))?;
        assert_eq!(observed_id(&after),Some("newest"));
        match after {LegacyDeliveryHeadObservation::Present{source}=>assert_eq!(source.availability,LegacyDeliverySourceAvailability::Pruned),_=>unreachable!()}
        let rows:u64=tx.query_row("SELECT COUNT(*) FROM input_legacy_delivery_current_sources WHERE agent_id='original' AND identity_incarnation=1",[],|r|r.get(0))?;
        assert_eq!(rows,2);
        let successor=InputEvidenceScope{agent_id:original.agent_id.clone(),identity_incarnation:2};
        let error=initialize_scope_tx(tx,&successor,None).unwrap_err();
        assert!(format!("{error:#}").contains("original continuity binding"));
        drop(before);drop(material);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn actual_original_import_version_shadows_fixed_id_across_work_associations_and_nul_text_is_nonempty(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("old.sqlite");
    {
        let mut connection = Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS.iter().filter(|m| m.version <= 90) {
            apply_migration(&mut connection, migration)?;
        }
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(&tx, &identity())?;
        insert_delivery_summary_evidence_tx(&tx, &summary("same-id", "fixed old", 0))?;
        tx.commit()?;
    }
    let db = RuntimeDb::open_and_migrate(path, home.path().join("old.lock"))?;
    let mut changed = summary("same-id", "\0nonempty", 1);
    changed.work_item_id = "changed-work".into();
    imported(&db, vec![changed])?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction_once(|tx| {
        initialize_scope_tx(tx, &scope(), None)?;
        let original = scope();
        let mut material = InputReadMaterial::new(&original, &request(), &ledger, (0, 0))?;
        let old = observe_head_tx(
            tx,
            &original,
            "original-work",
            LegacyDeliveryHeadSelector::LatestAny,
            |b, r| material.observe_projected(b, r),
        )?;
        assert_eq!(old, LegacyDeliveryHeadObservation::Absent);
        let new = observe_head_tx(
            tx,
            &original,
            "changed-work",
            LegacyDeliveryHeadSelector::LatestNonempty,
            |b, r| material.observe_projected(b, r),
        )?;
        assert_eq!(observed_id(&new), Some("same-id"));
        let fingerprint = fingerprint_on(
            tx,
            &original.agent_id,
            &Key::new(Family::LegacyDelivery, "same-id"),
        )?
        .context("actual stored fingerprint")?;
        let digest = qualify_metadata_tx(
            tx,
            &original,
            &Key::new(Family::LegacyDelivery, "same-id"),
            &fingerprint,
            |b, r| material.observe_projected(b, r),
        )?;
        assert_eq!(digest.len(), 64);
        drop(old);
        drop(new);
        drop(digest);
        drop(fingerprint);
        drop(material);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn missing_original_delivery_table_is_unavailable_not_observed_empty() -> Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("old.sqlite");
    {
        let mut connection = Connection::open(&path)?;
        ensure_migration_table(&connection)?;
        for migration in MIGRATIONS.iter().filter(|m| m.version <= 90) {
            apply_migration(&mut connection, migration)?;
        }
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(&tx, &identity())?;
        tx.execute_batch("DROP TABLE delivery_summaries;")?;
        tx.commit()?;
    }
    let db = RuntimeDb::open_and_migrate(path, home.path().join("old.lock"))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction_once(|tx| {
        initialize_scope_tx(tx, &scope(), None)?;
        let original = scope();
        let mut material = InputReadMaterial::new(&original, &request(), &ledger, (0, 0))?;
        let head = observe_head_tx(
            tx,
            &original,
            "original-work",
            LegacyDeliveryHeadSelector::LatestAny,
            |b, r| material.observe_projected(b, r),
        )?;
        assert_eq!(head, LegacyDeliveryHeadObservation::Unavailable);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn delivery_head_raw_callback_precedes_bad_source_json_and_native_callback_shares_one_allowance(
) -> Result<()> {
    for native in [false, true] {
        let (_home, db) = fixture()?;
        imported(&db, vec![summary("accepted", "text", 0)])?;
        db.transaction(|tx|{
            initialize_scope_tx(tx,&scope(),None)?;
            let raw=if native {serde_json::to_string(&serde_json::json!({"padding":vec![serde_json::Value::Null;9000]}))?}
                else {"{".to_owned()+&"x".repeat(64*1024)};
            tx.execute("UPDATE input_legacy_delivery_current_sources SET fingerprint_json=?1 WHERE record_id='accepted'",[raw])?;Ok(())
        })?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let mut count = 0usize;
        let result: Result<()> = db.transaction_once(|tx| {
            let original = scope();
            let mut limits = request();
            if !native {
                limits.byte_limit = 16 * 1024;
            }
            let mut material = InputReadMaterial::new(&original, &limits, &ledger, (0, 0))?;
            observe_head_tx(
                tx,
                &original,
                "original-work",
                LegacyDeliveryHeadSelector::LatestAny,
                |b, r| {
                    count += 1;
                    material.observe_projected(b, r)
                },
            )?;
            Ok(())
        });
        let error = result.unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(
            count, 1,
            "raw callback first; native helper/cumulative limit rejects before Source Strings"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
