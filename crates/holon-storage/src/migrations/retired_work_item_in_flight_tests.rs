use super::*;

#[test]
fn opening_legacy_database_normalizes_work_item_in_flight_once() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite3");
    let migration = |version| {
        MIGRATIONS
            .iter()
            .find(|migration| migration.version == version)
            .unwrap()
    };
    {
        let db = Connection::open(&path)?;
        ensure_migration_table(&db)?;
        db.execute_batch(migration(6).sql)?;
        db.execute_batch(migration(41).sql)?;
        db.execute_batch(migration(113).sql)?;

        for (agent, work, attempt) in [
            ("ordinary", "w1", "open-ordinary"),
            ("exact", "w2", "open-exact"),
            ("recovery", "w3", "open-recovery"),
            ("settled", "w4", "settled"),
            ("missing", "w5", "missing"),
        ] {
            let payload = serde_json::json!({
                "source_revision": 9,
                "state": {"state": "in_flight", "generation": 7, "attempt_id": attempt}
            });
            db.execute(
                "INSERT INTO execution_protocol_work_items VALUES (?1, ?2, 9, 7, 'in_flight', ?3)",
                params![agent, work, payload.to_string()],
            )?;
            if agent == "missing" {
                continue;
            }
            let source = if agent == "exact" {
                serde_json::json!({"kind": "triggered_wait", "wait_id": "resolved"})
            } else {
                serde_json::json!({"kind": "work_item_continuation", "work_item_id": work})
            };
            let payload = serde_json::json!({
                "binding": {"kind": "work_item", "work_item_id": work},
                "admitted_fences": {"work_item_generation": 7}
            });
            db.execute(
                "INSERT INTO execution_protocol_attempts (
                     agent_id, attempt_id, lifecycle_state, source_identity_json,
                     source_generation, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
                params![
                    agent,
                    attempt,
                    if agent == "settled" {
                        "settled"
                    } else {
                        "open"
                    },
                    source.to_string(),
                    payload.to_string()
                ],
            )?;
        }
        db.execute_batch(
            "INSERT INTO wait_conditions (
                 wait_condition_id, agent_id, work_item_id, status, kind, waiting_for,
                 created_at, updated_at, payload_json
             ) VALUES
                 ('resolved', 'exact', 'w2', 'resolved', 'timer', 'timer', 'a', 'a', '{}'),
                 ('active', 'recovery', 'w3', 'active', 'timer', 'timer', 'a', 'a', '{}'),
                 ('other', 'stranger', 'w1', 'active', 'timer', 'timer', 'a', 'a', '{}');
             INSERT INTO execution_protocol_work_items VALUES (
                 'unrelated', 'w6', 9, 7, 'paused',
                 '{\"source_revision\":9,\"state\":{\"state\":\"paused\",\"generation\":7,\"reason\":\"manual\"}}'
             );",
        )?;
    }
    for _ in 0..2 {
        let mut db = Connection::open(&path)?;
        apply_migration(&mut db, migration(119), unexpected_runtime_backfill)?;
        for (work, generation, state, wait_id) in [
            ("w1", 7, "runnable", None),
            ("w2", 7, "waiting", Some("resolved")),
            ("w3", 7, "waiting", Some("active")),
            ("w4", 8, "runnable", None),
            ("w5", 8, "runnable", None),
            ("w6", 7, "paused", None),
        ] {
            let (indexed_revision, indexed_generation, indexed_state, payload): (
                u64,
                u64,
                String,
                String,
            ) = db.query_row(
                "SELECT source_revision, generation, lifecycle_state, payload_json
                 FROM execution_protocol_work_items WHERE work_item_id = ?1",
                [work],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            assert_eq!(
                (indexed_revision, indexed_generation, indexed_state.as_str()),
                (9, generation, state),
                "{work}"
            );
            let record: WorkItemExecutionRecord = serde_json::from_str(&payload)?;
            assert_eq!(record.source_revision, 9);
            assert_eq!(record.generation(), generation);
            match record.state {
                WorkItemExecutionState::Runnable { recovery_ref, .. } => {
                    assert_eq!(state, "runnable");
                    assert_eq!(recovery_ref, None);
                }
                WorkItemExecutionState::Waiting { wait, .. } => {
                    assert_eq!(Some(wait.wait_id.as_str()), wait_id);
                }
                WorkItemExecutionState::Paused { reason, .. } => assert_eq!(reason, "manual"),
                _ => panic!("unexpected state for {work}"),
            }
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM execution_protocol_attempts WHERE lifecycle_state = 'open'",
                [],
                |row| row.get::<_, u64>(0)
            )?,
            3
        );
        assert!(
            db.execute(
                "UPDATE execution_protocol_work_items SET lifecycle_state = 'in_flight' WHERE work_item_id = 'w1'",
                []
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn reopening_with_retired_work_execution_rebinds_only_matching_scoped_proofs() -> Result<()> {
    use serde_json::{json, Value};

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite3");
    let old_payload =
        r#"{"source_revision":9,"state":{"state":"in_flight","generation":7,"attempt_id":"lost"}}"#;
    let fingerprint = |marker, payload: &str| {
        json!({
            "content_hash": null, "sequence": null, "turn_index": null,
            "created_at": "", "owner_kind": null, "owner_id": null,
            "revision_marker": marker, "payload_bytes": payload.len(), "related_turn_id": null,
        })
    };
    let old = fingerprint("9:7:in_flight", old_payload);
    let digest = bytes_sha256_hex(old_payload.as_bytes());
    let mut old_fixed = old.clone();
    old_fixed["frozen_payload_digest"] = json!(digest);
    let mut stale_fixed = old_fixed.clone();
    stale_fixed["frozen_payload_digest"] = json!("wrong-digest");
    {
        let db = Connection::open(&path)?;
        ensure_migration_table(&db)?;
        for version in [41, 113] {
            db.execute_batch(
                MIGRATIONS
                    .iter()
                    .find(|m| m.version == version)
                    .unwrap()
                    .sql,
            )?;
        }
        db.execute_batch(schema::EPISODE_CONTINUITY_SCHEMA)?;
        db.execute_batch(schema::INPUT_EVIDENCE_SCHEMA)?;
        db.execute(
            "INSERT INTO execution_protocol_work_items VALUES ('owner', 'work', 9, 7, 'in_flight', ?1)",
            [old_payload],
        )?;
        for (seed, agent) in [("owned", "owner"), ("foreign", "other")] {
            db.execute(
                "INSERT INTO episode_continuity_seeds
                 (seed_id, agent_id, policy_version, boundary_id, source_kind, roots_json,
                  candidate_payloads_json, import_required, captured_at)
                 VALUES (?1, ?2, 1, 'boundary', 'upgrade_unbound', '{}', '[]', 0, 'a')",
                params![seed, agent],
            )?;
            db.execute(
                "INSERT INTO input_evidence_eligibility VALUES
                 (?1, 'upgrade', 'work_execution', 'work', ?2, 1)",
                params![seed, old_fixed.to_string()],
            )?;
            db.execute(
                "INSERT INTO input_evidence_seed_recovery_current
                 (seed_id, family, record_id, fingerprint_json, revision, origin_kind)
                 VALUES (?1, 'work_execution', 'work', ?2, 4, 'fixed_member')",
                params![seed, old_fixed.to_string()],
            )?;
        }
        db.execute(
            "INSERT INTO input_evidence_eligibility VALUES
             ('owned', 'first_import', 'work_execution', 'work', ?1, 1)",
            [stale_fixed.to_string()],
        )?;
        for (agent, incarnation, digest) in [
            ("owner", 1, digest.as_str()),
            ("owner", 2, "wrong-digest"),
            ("other", 1, digest.as_str()),
        ] {
            db.execute(
                "INSERT INTO input_evidence_current
                 (agent_id, identity_incarnation, family, record_id, revision, content_digest,
                  fingerprint_json, availability, qualification)
                 VALUES (?1, ?2, 'work_execution', 'work', 5, ?3, ?4, 'present', 'committed_writer')",
                params![agent, incarnation, digest, old.to_string()],
            )?;
        }
    }
    let migration = MIGRATIONS.iter().find(|m| m.version == 119).unwrap();
    for _ in 0..2 {
        let mut db = Connection::open(&path)?;
        apply_migration(&mut db, migration, unexpected_runtime_backfill)?;
        let payload: String = db.query_row(
            "SELECT payload_json FROM execution_protocol_work_items WHERE work_item_id = 'work'",
            [],
            |row| row.get(0),
        )?;
        let new = fingerprint("9:8:runnable", &payload);
        let new_digest = bytes_sha256_hex(payload.as_bytes());
        let mut new_fixed = new.clone();
        new_fixed["frozen_payload_digest"] = json!(new_digest);
        for (agent, incarnation, hash, expected) in [
            ("owner", 1, new_digest.as_str(), &new),
            ("owner", 2, "wrong-digest", &old),
            ("other", 1, digest.as_str(), &old),
        ] {
            let (actual_hash, actual, revision): (String, String, i64) = db.query_row(
                "SELECT content_digest, fingerprint_json, revision FROM input_evidence_current
                 WHERE agent_id = ?1 AND identity_incarnation = ?2",
                params![agent, incarnation],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(actual_hash, hash);
            assert_eq!(serde_json::from_str::<Value>(&actual)?, *expected);
            assert_eq!(revision, 5);
        }
        for (query, expected) in [
            ("SELECT fingerprint_json FROM input_evidence_eligibility WHERE seed_id='owned' AND capture_phase='upgrade'", &new_fixed),
            ("SELECT fingerprint_json FROM input_evidence_eligibility WHERE seed_id='owned' AND capture_phase='first_import'", &stale_fixed),
            ("SELECT fingerprint_json FROM input_evidence_eligibility WHERE seed_id='foreign'", &old_fixed),
            ("SELECT fingerprint_json FROM input_evidence_seed_recovery_current WHERE seed_id='owned' AND revision=4 AND origin_kind='fixed_member'", &new_fixed),
            ("SELECT fingerprint_json FROM input_evidence_seed_recovery_current WHERE seed_id='foreign'", &old_fixed),
        ] {
            let actual: String = db.query_row(query, [], |row| row.get(0))?;
            assert_eq!(serde_json::from_str::<Value>(&actual)?, *expected);
        }
    }
    Ok(())
}
