use super::*;

const OUTCOME: &str = "lineage-outcome-0";

fn state_dir(directory: &tempfile::TempDir) -> std::path::PathBuf {
    directory.path().join(".holon/state")
}

fn open(directory: &tempfile::TempDir) -> Result<RuntimeDb> {
    RuntimeDb::open_and_migrate(
        state_dir(directory).join("runtime.sqlite"),
        state_dir(directory).join("runtime.lock"),
    )
}

fn bootstrap(directory: &tempfile::TempDir) -> Result<crate::runtime::RuntimeHandle> {
    crate::runtime::RuntimeHandle::new(
        "agent-a",
        directory.path().to_path_buf(),
        directory.path().to_path_buf(),
        String::new(),
        std::sync::Arc::new(crate::provider::StubProvider::new("unused")),
        "agent-a".into(),
        crate::context::ContextConfig::default(),
    )
}

type StoredAliases = Vec<(i64, i64, String, String)>;

fn fixture(old_v115: bool) -> Result<(tempfile::TempDir, InputEvidenceLoadRequest, StoredAliases)> {
    let directory = tempfile::tempdir()?;
    let db = open(&directory)?;
    let request = db.transaction(|tx| {
        tx.execute_batch(INPUT_EVIDENCE_SCHEMA)?;
        capture_input_evidence_upgrade_tx(tx, time())?;
        upsert_agent_identity_tx(tx, &identity())?;
        let mut state = AgentState::new("agent-a");
        state.pending = 1;
        upsert_agent_state_tx(tx, &state)?;
        // Both are data-only migrations. Unrecord them before the writer
        // writes on this otherwise empty schema so the reopen really runs them.
        tx.execute(
            "DELETE FROM schema_migrations WHERE version IN (115, 116)",
            [],
        )?;
        let source = install_provider_lineage_with_legacy_outcome(tx, true)?;
        crate::runtime_db::repositories::upsert_queue_entry_tx(
            tx,
            &crate::types::QueueEntryRecord {
                message_id: source.id.clone(),
                agent_id: source.agent_id.clone(),
                priority: source.priority.clone(),
                status: crate::types::QueueEntryStatus::Queued,
                created_at: source.created_at,
                updated_at: source.created_at,
            },
        )?;
        let key = Key::new(Family::Outcome, OUTCOME);
        let alias = alias_on(tx, &scope(), &key)?.context("writer did not qualify Outcome")?;
        assert_eq!(
            alias.fingerprint,
            fingerprint_on(tx, "agent-a", &key)?.unwrap()
        );
        assert_eq!(
            alias.digest.as_deref(),
            Some(bytes_digest(payload_on(tx, "agent-a", &key)?.unwrap().as_bytes()).as_str())
        );
        assert!(alias.fingerprint.frozen_payload_digest.is_none());
        tx.execute(
            "INSERT INTO input_evidence_current
             SELECT agent_id, 2, family, record_id, revision, content_digest,
                    fingerprint_json, attempt_turn_proof_json, availability, qualification, seed_id
             FROM input_evidence_current
             WHERE agent_id='agent-a' AND identity_incarnation=1 AND family='outcome'",
            [],
        )?;
        Ok(provider_lineage_request(&source))
    })?;
    let before = outcome_aliases(&db, "lineage-outcome-1")?;
    drop(db);
    if old_v115 {
        let mut connection = Connection::open(state_dir(&directory).join("runtime.sqlite"))?;
        let migration = MIGRATIONS.iter().find(|m| m.version == 115).unwrap();
        apply_migration(&mut connection, migration)?;
        // Recreate the only divergent write in old, already-recorded v115.
        connection.execute(
            "UPDATE input_evidence_current
             SET fingerprint_json=json_set(fingerprint_json,'$.frozen_payload_digest',content_digest)
             WHERE agent_id='agent-a' AND family='outcome' AND record_id=?1",
            [OUTCOME],
        )?;
    }
    Ok((directory, request, before))
}

fn outcome_aliases(db: &RuntimeDb, id: &str) -> Result<StoredAliases> {
    db.transaction(|tx| {
        let mut statement = tx.prepare(
            "SELECT identity_incarnation, revision, content_digest, fingerprint_json
             FROM input_evidence_current WHERE agent_id='agent-a' AND family='outcome'
               AND record_id=?1 ORDER BY identity_incarnation",
        )?;
        let rows =
            statement.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    })
}

#[tokio::test]
async fn retired_outcome_upgrade_preserves_current_bootstrap_and_is_idempotent() -> Result<()> {
    for already_recorded in [false, true] {
        let (directory, request, unaffected) = fixture(already_recorded)?;
        let db = open(&directory)?;
        let repaired = outcome_aliases(&db, OUTCOME)?;
        assert_eq!(repaired.len(), 2);
        assert_eq!(outcome_aliases(&db, "lineage-outcome-1")?, unaffected);
        db.transaction(|tx| {
            let key = Key::new(Family::Outcome, OUTCOME);
            let expected = fingerprint_on(tx, "agent-a", &key)?.context("canonical fingerprint")?;
            let payload = payload_on(tx, "agent-a", &key)?.context("migrated Outcome")?;
            assert!(payload.contains("\"kind\":\"interrupted\""));
            for (index, (incarnation, revision, digest, fingerprint)) in repaired.iter().enumerate()
            {
                assert_eq!(*incarnation, index as i64 + 1);
                assert_eq!(*revision, 1);
                assert_eq!(digest, &bytes_digest(payload.as_bytes()));
                assert_eq!(serde_json::from_str::<Fingerprint>(fingerprint)?, expected);
            }
            // Same Current Input preparation invoked by the runtime storage
            // constructor, with the terminal Outcome reached from a pending source.
            let captured = current(prepare_input_evidence_snapshot_tx(tx, &scope(), &request)?)
                .resident
                .capture()?;
            assert!(captured
                .records
                .iter()
                .any(|record| record.locator.key == key));
            Ok(())
        })?;
        drop(db);
        let runtime = bootstrap(&directory)?;
        assert_eq!(runtime.agent_state().await?.pending, 1);
        drop(runtime);
        assert_eq!(outcome_aliases(&open(&directory)?, OUTCOME)?, repaired);
    }
    Ok(())
}

#[tokio::test]
async fn retired_outcome_upgrade_does_not_qualify_bad_evidence() -> Result<()> {
    for damage in [
        "UPDATE execution_protocol_outcomes SET payload_json=replace(payload_json,'provider failed','provider FAILED') WHERE outcome_id=?1",
        "UPDATE input_evidence_current SET content_digest='wrong' WHERE record_id=?1 AND identity_incarnation=1",
        "UPDATE input_evidence_current SET fingerprint_json=json_set(fingerprint_json,'$.revision_marker','wrong') WHERE record_id=?1 AND identity_incarnation=1",
        "UPDATE input_evidence_current SET fingerprint_json=json_set(fingerprint_json,'$.payload_bytes',1) WHERE record_id=?1 AND identity_incarnation=1",
    ] {
        let (directory, request, _) = fixture(true)?;
        let connection = Connection::open(state_dir(&directory).join("runtime.sqlite"))?;
        connection.execute(damage, [OUTCOME])?;
        drop(connection);
        let db = open(&directory)?;
        let err = db.transaction(|tx| {
            prepare_input_evidence_snapshot_tx(tx, &scope(), &request).map(|_| ())
        }).expect_err("damaged Outcome must not be admitted");
        assert!(
            matches!(err.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == OUTCOME),
            "{err:#}"
        );
        drop(db);
        let err = bootstrap(&directory)
            .err()
            .context("runtime bootstrap must reject the bad Outcome")?;
        assert!(
            matches!(err.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == OUTCOME),
            "{err:#}"
        );
    }
    Ok(())
}
