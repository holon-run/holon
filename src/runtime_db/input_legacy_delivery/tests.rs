use super::*;
use crate::{
    runtime_db::{
        evidence::{
            insert_delivery_summary_evidence_tx, upsert_agent_identity_tx, upsert_agent_state_tx,
        },
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
    },
    types::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use rusqlite::Connection;

fn old_db() -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= 90)
    {
        apply_migration(&mut connection, migration)?;
    }
    Ok(connection)
}
fn identity(agent: &str) -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        agent,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}
fn summary(id: &str, text: &str) -> DeliverySummaryRecord {
    let mut record = DeliverySummaryRecord::new("original", "work-original", text, Some(3), None);
    record.id = id.into();
    record.turn_id = Some("original-turn".into());
    record.created_at = "2026-10-14T01:00:00.000000123Z".parse().unwrap();
    record
}
fn upgrade(connection: &mut Connection) -> Result<()> {
    apply_migration(
        connection,
        MIGRATIONS
            .iter()
            .find(|m| m.version == 91)
            .context("migration absent")?,
    )
}

#[test]
fn legacy_delivery_upgrade_streams_over_native_record_cap_and_releases_scratch() -> Result<()> {
    let mut connection = old_db()?;
    {
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(&tx, &identity("original"))?;
        for index in 0..100_001 {
            insert_delivery_summary_evidence_tx(
                &tx,
                &summary(&format!("delivery-{index:06}"), "original"),
            )?;
        }
        tx.commit()?;
    }
    connection.execute_batch(LEGACY_DELIVERY_SCHEMA)?;
    let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
    let report = {
        let tx = connection.transaction()?;
        let report = capture_upgrade_with_scratch_tx(&tx, &scratch)?;
        tx.commit()?;
        report
    };
    assert_eq!(report.members, 100_001);
    assert!(report.members > u64::try_from(MAX_RECORDS)?);
    assert!(report.peak_owned_estimate_bytes > 4096);
    assert!(
        report.peak_owned_estimate_bytes < 512 * 1024,
        "one-row owned scratch: {report:?}"
    );
    assert_eq!(scratch.report()?.observed_bytes, 0);
    let (count, digest_bytes, sealed, exact_inc): (u64, usize, bool, bool) = connection.query_row(
        "SELECT c.member_count,length(c.metadata_digest),c.sealed,NOT EXISTS(SELECT 1 FROM input_legacy_delivery_fixed_members WHERE identity_incarnation!=1 OR seed_id IS NOT NULL) FROM input_legacy_delivery_captures c WHERE capture_id=?1",
        [UPGRADE], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
    assert_eq!(
        (count, digest_bytes, sealed, exact_inc),
        (100_001, 64, true, true)
    );
    let stored: String = connection.query_row("SELECT fingerprint_json FROM input_legacy_delivery_fixed_members WHERE record_id='delivery-000000'",[],|row|row.get(0))?;
    let fingerprint: Fingerprint = serde_json::from_str(&stored)?;
    assert_eq!(fingerprint.owner_kind.as_deref(), Some("delivery_summary"));
    assert_eq!(fingerprint.owner_id.as_deref(), Some("work-original"));
    assert_eq!(
        fingerprint.related_turn_id.as_deref(),
        Some("original-turn")
    );
    assert!(fingerprint.frozen_payload_digest.is_some());
    // A repeated fixed capture neither rescans malformed raw tails nor adds a
    // later identity incarnation to the original cohort.
    connection.execute(
        "UPDATE delivery_summaries SET payload_json='{' WHERE evidence_id='delivery-000000'",
        [],
    )?;
    connection.execute("INSERT INTO delivery_summaries(evidence_id,agent_id,created_at,kind,payload_json) VALUES('late-raw','original','later','delivery_summary','{')",[])?;
    let tx = connection.transaction()?;
    let repeated = capture_upgrade_with_scratch_tx(&tx, &scratch)?;
    assert_eq!(repeated.members, 100_001);
    assert_eq!(repeated.peak_owned_estimate_bytes, 0);
    let late: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM input_legacy_delivery_fixed_members WHERE record_id='late-raw')",[],|row|row.get(0))?;
    assert!(!late);
    tx.commit()?;
    assert_eq!(scratch.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn legacy_delivery_real_upgrade_bad_row_rolls_back_entire_cutover_then_retry_seals() -> Result<()> {
    let mut connection = old_db()?;
    {
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(&tx, &identity("original"))?;
        insert_delivery_summary_evidence_tx(&tx, &summary("a-good", "good"))?;
        insert_delivery_summary_evidence_tx(&tx, &summary("z-bad", "bad"))?;
        tx.execute(
            "UPDATE delivery_summaries SET work_item_id='foreign-work' WHERE evidence_id='z-bad'",
            [],
        )?;
        tx.commit()?;
    }
    let error = upgrade(&mut connection).unwrap_err();
    assert!(
        format!("{error:#}").contains("payload address/time differs"),
        "{error:#}"
    );
    let (version, schema): (bool,bool) = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=91),EXISTS(SELECT 1 FROM sqlite_master WHERE name='input_legacy_delivery_fixed_members')",
        [],|row|Ok((row.get(0)?,row.get(1)?)))?;
    assert_eq!((version, schema), (false, false));
    {
        let tx = connection.transaction()?;
        insert_delivery_summary_evidence_tx(&tx, &summary("z-bad", "bad"))?;
        tx.commit()?;
    }
    upgrade(&mut connection)?;
    let (count, sealed): (u64, bool) = connection.query_row(
        "SELECT member_count,sealed FROM input_legacy_delivery_captures WHERE capture_id=?1",
        [UPGRADE],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((count, sealed), (2, true));
    Ok(())
}

#[test]
fn legacy_delivery_scalar_gate_precedes_bad_json_and_native_nodes_precede_serde() -> Result<()> {
    for (native, identity_body) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut connection = old_db()?;
        connection.execute_batch(LEGACY_DELIVERY_SCHEMA)?;
        {
            let tx = connection.transaction()?;
            insert_delivery_summary_evidence_tx(&tx, &summary("original-row", "old"))?;
            upsert_agent_identity_tx(&tx, &identity("original"))?;
            if identity_body {
                let payload = if native {
                    let mut identity = serde_json::to_value(identity("original"))?;
                    identity["unknown_node_material"] =
                        serde_json::Value::Array(vec![serde_json::Value::Null; 9000]);
                    serde_json::to_string(&identity)?
                } else {
                    "{".repeat(9 * 1024 * 1024)
                };
                tx.execute(
                    "UPDATE agent_identities SET payload_json=?1 WHERE agent_id='original'",
                    [payload],
                )?;
            } else if native {
                let mut record = summary("original-row", "old");
                record.evidence = Some(serde_json::Value::Array(vec![
                    serde_json::Value::Null;
                    9000
                ]));
                insert_delivery_summary_evidence_tx(&tx, &record)?;
            } else {
                tx.execute("UPDATE delivery_summaries SET payload_json=?1 WHERE evidence_id='original-row'",["{".repeat(9*1024*1024)])?;
            }
            tx.commit()?;
        }
        let scratch = InputEvidenceBudgetLedger::new(Default::default())?;
        let error = {
            let tx = connection.transaction()?;
            capture_upgrade_with_scratch_tx(&tx, &scratch).unwrap_err()
        };
        let expected = if native {
            "LegacyDelivery fixed native"
        } else {
            "LegacyDelivery fixed raw"
        };
        assert!(
            matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::ResourceExhausted(label)) if *label==expected),
            "{error:#}"
        );
        assert_eq!(scratch.report()?.observed_bytes, 0);
        let (captures,members): (u64,u64) = connection.query_row("SELECT (SELECT COUNT(*) FROM input_legacy_delivery_captures),(SELECT COUNT(*) FROM input_legacy_delivery_fixed_members)",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!((captures, members), (0, 0));
    }
    Ok(())
}

#[test]
fn legacy_delivery_cutover_preserves_scope_full_body_and_two_distinct_head_orders() -> Result<()> {
    let mut connection = old_db()?;
    let original = summary("older-nonempty", "original text");
    let mut empty = summary("newer-empty", "");
    empty.created_at += chrono::Duration::milliseconds(1);
    let mut foreign = summary("foreign", "foreign text");
    foreign.agent_id = "foreign".into();
    {
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(&tx, &identity("original"))?;
        upsert_agent_identity_tx(&tx, &identity("foreign"))?;
        for record in [&original, &empty, &foreign] {
            insert_delivery_summary_evidence_tx(&tx, record)?;
        }
        tx.commit()?;
    }
    upgrade(&mut connection)?;
    let latest_any: String = connection.query_row("SELECT record_id FROM input_legacy_delivery_fixed_members WHERE agent_id='original' AND identity_incarnation=1 AND work_item_id='work-original' ORDER BY created_at DESC,record_id DESC LIMIT 1",[],|row|row.get(0))?;
    let latest_nonempty: String = connection.query_row("SELECT record_id FROM input_legacy_delivery_fixed_members WHERE agent_id='original' AND identity_incarnation=1 AND work_item_id='work-original' AND text_nonempty=1 ORDER BY created_at DESC,record_id DESC LIMIT 1",[],|row|row.get(0))?;
    assert_eq!(latest_any, empty.id);
    assert_eq!(latest_nonempty, original.id);
    let encoded: String = connection.query_row(
        "SELECT payload_json FROM delivery_summaries WHERE evidence_id=?1",
        [&original.id],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<DeliverySummaryRecord>(&encoded)?,
        original
    );
    let foreign_same_inc: u64 = connection.query_row("SELECT COUNT(*) FROM input_legacy_delivery_fixed_members WHERE agent_id='original' AND identity_incarnation=2",[],|row|row.get(0))?;
    assert_eq!(foreign_same_inc, 0);
    Ok(())
}

#[test]
fn legacy_delivery_sealed_header_and_member_replacement_cannot_reopen_cohort() -> Result<()> {
    let mut connection = old_db()?;
    {
        let tx = connection.transaction()?;
        insert_delivery_summary_evidence_tx(&tx, &summary("original-row", "old"))?;
        tx.commit()?;
    }
    upgrade(&mut connection)?;
    connection.execute("INSERT INTO input_legacy_delivery_captures(capture_id,table_available,member_count,metadata_digest,sealed) VALUES('staging',1,0,'',0)",[])?;
    connection.execute("INSERT INTO input_legacy_delivery_fixed_members(capture_id,record_id,agent_id,identity_incarnation,seed_id,work_item_id,created_at,text_nonempty,fingerprint_json,content_digest) SELECT 'staging','staging-row',agent_id,identity_incarnation,seed_id,work_item_id,created_at,text_nonempty,fingerprint_json,content_digest FROM input_legacy_delivery_fixed_members WHERE record_id='original-row'",[])?;
    for sql in [
        "INSERT OR REPLACE INTO input_legacy_delivery_captures(capture_id,table_available,member_count,metadata_digest,sealed) VALUES('legacy_delivery/v1/upgrade',1,0,'',0)",
        "UPDATE OR REPLACE input_legacy_delivery_captures SET capture_id='legacy_delivery/v1/upgrade' WHERE capture_id='staging'",
        "DELETE FROM input_legacy_delivery_captures WHERE capture_id='legacy_delivery/v1/upgrade'",
        "UPDATE input_legacy_delivery_fixed_members SET capture_id='staging' WHERE record_id='original-row'",
        "UPDATE input_legacy_delivery_fixed_members SET capture_id='legacy_delivery/v1/upgrade' WHERE record_id='staging-row'",
        "INSERT OR REPLACE INTO input_legacy_delivery_fixed_members SELECT * FROM input_legacy_delivery_fixed_members WHERE record_id='original-row'",
        "DELETE FROM input_legacy_delivery_fixed_members WHERE record_id='original-row'",
    ] { assert!(connection.execute_batch(sql).is_err(),"sealed fence: {sql}"); }
    let (count, sealed): (u64, bool) = connection.query_row(
        "SELECT member_count,sealed FROM input_legacy_delivery_captures WHERE capture_id=?1",
        [UPGRADE],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((count, sealed), (1, true));
    let count: u64 = connection.query_row(
        "SELECT COUNT(*) FROM input_legacy_delivery_fixed_members WHERE capture_id=?1",
        [UPGRADE],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn legacy_delivery_missing_table_is_frozen_unavailable_without_raw_fallback() -> Result<()> {
    let mut connection = old_db()?;
    connection.execute_batch("DROP TABLE delivery_summaries")?;
    upgrade(&mut connection)?;
    let (available,count,sealed): (bool,u64,bool) = connection.query_row("SELECT table_available,member_count,sealed FROM input_legacy_delivery_captures WHERE capture_id=?1",[UPGRADE],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    assert_eq!((available, count, sealed), (false, 0, true));
    Ok(())
}

#[test]
fn legacy_delivery_v81_unbound_member_retains_only_original_seed_and_full_time() -> Result<()> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
        apply_migration(&mut connection, migration)?;
    }
    let original = summary("old-unbound", "original");
    {
        let tx = connection.transaction()?;
        upsert_agent_state_tx(&tx, &crate::types::AgentState::new("original"))?;
        insert_delivery_summary_evidence_tx(&tx, &original)?;
        tx.commit()?;
    }
    for migration in MIGRATIONS.iter().filter(|m| m.version > 81) {
        apply_migration(&mut connection, migration)?;
    }
    let (inc,seed,source,unbound):(Option<u64>,Option<String>,String,bool)=connection.query_row(
        "SELECT m.identity_incarnation,m.seed_id,s.source_kind,s.identity_incarnation IS NULL FROM input_legacy_delivery_fixed_members m JOIN episode_continuity_seeds s ON s.seed_id=m.seed_id WHERE m.record_id='old-unbound'",
        [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
    assert!(inc.is_none() && seed.is_some() && unbound);
    assert_eq!(source, "upgrade_unbound");
    let original_payload: String = connection.query_row(
        "SELECT payload_json FROM delivery_summaries WHERE evidence_id='old-unbound'",
        [],
        |row| row.get(0),
    )?;
    let fixed_digest:String=connection.query_row("SELECT content_digest FROM input_legacy_delivery_fixed_members WHERE record_id='old-unbound'",[],|row|row.get(0))?;
    assert_eq!(bytes_digest(original_payload.as_bytes()), fixed_digest);
    assert_eq!(
        serde_json::from_str::<DeliverySummaryRecord>(&original_payload)?,
        original
    );
    Ok(())
}
