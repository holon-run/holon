use super::*;

#[test]
fn actual_foundation_verifier_persists_all_five_sql_error_branches_as_false() -> Result<()> {
    for (alter, capabilities) in [
        (
            "ALTER TABLE audit_events RENAME COLUMN data_json TO saved_data_json",
            vec![EVENT_PROJECTION_EFFECT_COMPLETE, BRIEF_ATOMIC_LINKAGE_VERIFIED],
        ),
        (
            "ALTER TABLE briefs RENAME COLUMN preview TO saved_preview",
            vec![ROSTER_SNAPSHOT_VERIFIED, PROJECTION_SNAPSHOT_VERIFIED],
        ),
        (
            "ALTER TABLE conversation_source_revisions RENAME COLUMN source_kind TO saved_source_kind",
            vec![CONVERSATION_READ_VERIFIED],
        ),
    ] {
        let mut connection = migrated_connection()?;
        SYNC.verify_observer_sync_foundations(&mut connection)?;
        connection.execute(
            "UPDATE observer_sync_capability_verifications SET verification_version = 0
             WHERE capability = ?1",
            [EVENT_PROJECTION_EFFECT_COMPLETE],
        )?;
        connection.execute_batch(alter)?;
        SYNC.verify_observer_sync_foundations(&mut connection)?;
        assert!(connection.is_autocommit());
        for capability in capabilities {
            let (verified, detail): (bool, String) = connection.query_row(
                "SELECT verified, detail FROM observer_sync_capability_verifications WHERE capability = ?1",
                [capability],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert!(!verified, "{capability}");
            let detail: serde_json::Value = serde_json::from_str(&detail)?;
            assert!(detail["error"].as_str().is_some_and(|error| !error.is_empty()), "{capability}: {detail}");
            if capability == EVENT_PROJECTION_EFFECT_COMPLETE {
                assert_eq!(detail["source"], "full_inventory");
                assert_eq!(detail.as_object().unwrap().len(), 2);
            } else {
                assert_eq!(detail.as_object().unwrap().len(), 1);
            }
        }
    }
    Ok(())
}
