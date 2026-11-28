use super::*;

use serde_json::{json, Value};

#[test]
fn retire_destructive_operation_scheduler_accepts_compacted_baselines() -> Result<()> {
    let migration = MIGRATIONS
        .iter()
        .find(|migration| migration.name == "retire_destructive_operation_scheduler")
        .unwrap();
    for has_metadata in [false, true] {
        let mut db = Connection::open_in_memory()?;
        ensure_migration_table(&db)?;
        db.execute_batch(
            "INSERT INTO schema_migrations VALUES (120, 'turn_content_deletion', 'fixture');
             CREATE TABLE destructive_operations (operation_id TEXT PRIMARY KEY);",
        )?;
        if has_metadata {
            db.execute_batch(
                "CREATE TABLE runtime_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO runtime_metadata VALUES
                    ('destructive_operation_closed:old', 'fixture'),
                    ('unrelated', 'preserved');",
            )?;
        }

        for _ in 0..2 {
            apply_migration(&mut db, migration, unexpected_runtime_backfill)?;
            assert_eq!(current_schema_version(&db)?, migration.version);
            let tx = db.transaction()?;
            assert!(!table_exists_tx(&tx, "turn_records")?);
            assert!(!table_exists_tx(&tx, "destructive_operations")?);
            assert_eq!(table_exists_tx(&tx, "runtime_metadata")?, has_metadata);
            if has_metadata {
                let keys = tx
                    .prepare("SELECT key FROM runtime_metadata")?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(keys, ["unrelated"]);
            }
        }
    }
    Ok(())
}

/// Build a v117-era database: the Turn spine, frozen input evidence, and
/// completed turns that still carry the retired destructive-operation fence.
fn legacy_destructive_fence_database(path: &std::path::Path) -> Result<Connection> {
    let mut db = Connection::open(path)?;
    ensure_migration_table(&db)?;
    for version in [2, 5, 6, 8, 29, 41] {
        db.execute_batch(
            MIGRATIONS
                .iter()
                .find(|migration| migration.version == version)
                .unwrap()
                .sql,
        )?;
    }
    db.execute_batch(schema::EPISODE_CONTINUITY_SCHEMA)?;
    db.execute_batch(schema::INPUT_EVIDENCE_SCHEMA)?;
    // v52 adds the Turn owner columns through its migration hook.
    apply_migration(
        &mut db,
        MIGRATIONS
            .iter()
            .find(|migration| migration.version == 52)
            .unwrap(),
        unexpected_runtime_backfill,
    )?;
    db.execute_batch(
        "INSERT INTO schema_migrations VALUES (94, 'explicit_episode_continuity', 'fixture');
         INSERT INTO schema_migrations VALUES (96, 'qualified_input_evidence', 'fixture');
         INSERT INTO schema_migrations VALUES (117, 'agent_message_chain_budgets', 'fixture');",
    )?;
    Ok(db)
}

/// The runtime's Turn fingerprint projection, mirroring
/// `runtime_db::input_evidence_snapshot::fingerprint_on` for family `turn`.
fn turn_fingerprint(
    turn_index: i64,
    created_at: &str,
    owner_kind: Option<&str>,
    owner_id: Option<&str>,
    payload: &str,
) -> Value {
    json!({
        "content_hash": null,
        "sequence": null,
        "turn_index": turn_index,
        "created_at": created_at,
        "owner_kind": owner_kind,
        "owner_id": owner_id,
        "revision_marker": "canonical_body",
        "payload_bytes": payload.len(),
        "related_turn_id": null,
    })
}

fn frozen_turn_fingerprint(
    turn_index: i64,
    created_at: &str,
    owner_kind: Option<&str>,
    owner_id: Option<&str>,
    payload: &str,
) -> Value {
    let mut fingerprint = turn_fingerprint(turn_index, created_at, owner_kind, owner_id, payload);
    fingerprint["frozen_payload_digest"] = json!(bytes_sha256_hex(payload.as_bytes()));
    fingerprint
}

/// Snapshot every input-evidence row that family-scoped repairs may touch,
/// keyed by identity columns, so drift assertions stay a bounded diff.
fn snapshot_input_evidence(db: &Connection) -> Result<std::collections::BTreeMap<String, String>> {
    use std::collections::BTreeMap;
    let mut snapshot = BTreeMap::new();
    {
        let mut statement = db.prepare(
            "SELECT seed_id, capture_phase, family, record_id, fingerprint_json
             FROM input_evidence_eligibility",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                format!(
                    "eligibility|{}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?
                ),
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (key, value) = row?;
            snapshot.insert(key, value);
        }
    }
    {
        let mut statement = db.prepare(
            "SELECT seed_id, family, record_id, revision, fingerprint_json
             FROM input_evidence_seed_recovery_current",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                format!(
                    "seed_recovery|{}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?
                ),
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (key, value) = row?;
            snapshot.insert(key, value);
        }
    }
    {
        let mut statement = db.prepare(
            "SELECT agent_id, identity_incarnation, family, record_id, revision,
                    COALESCE(content_digest, ''), availability, fingerprint_json
             FROM input_evidence_current",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                format!(
                    "current|{}|{}|{}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(6)?
                ),
                format!(
                    "{}\u{1f}{}",
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(7)?
                ),
            ))
        })?;
        for row in rows {
            let (key, value) = row?;
            snapshot.insert(key, value);
        }
    }
    Ok(snapshot)
}

#[test]
fn repair_retired_destructive_operation_turn_fingerprints_refreezes_only_v121_drift() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite3");
    let migration = |version: i64| {
        MIGRATIONS
            .iter()
            .find(|migration| migration.version == version)
            .unwrap()
    };

    let fenced_payload = |fence: &str| {
        json!({
            "kind": "model_turn",
            "state": "settled",
            "destructive_operation_id": fence,
            "note": "v117-era turn",
        })
        .to_string()
    };
    let plain_payload = || {
        json!({
            "kind": "model_turn",
            "state": "settled",
            "note": "turn without a fence",
        })
        .to_string()
    };
    let legacy_work_payload =
        r#"{"source_revision":9,"state":{"state":"in_flight","generation":7,"attempt_id":"lost"}}"#;

    let victim_payload = fenced_payload("fence-op-1");
    let open_payload = fenced_payload("fence-op-2");
    let healthy_payload = plain_payload();
    let mismatch_payload = plain_payload();
    let stale_fence_payload = fenced_payload("fence-op-3");

    // Frozen fingerprints as of v117, before the v121 rewrite.
    let victim_frozen =
        frozen_turn_fingerprint(7, "2026-11-24T17:19:31Z", None, None, &victim_payload);
    let open_frozen = frozen_turn_fingerprint(
        8,
        "2026-11-25T03:00:00Z",
        Some("conversation"),
        Some("conv-1"),
        &open_payload,
    );
    let healthy_frozen = frozen_turn_fingerprint(
        3,
        "2026-11-23T08:00:00Z",
        Some("conversation"),
        Some("conv-1"),
        &healthy_payload,
    );
    // Non-v121 shapes: drift whose metadata no longer matches the row, and a
    // payload that still carries a live fence value.
    let mismatch_frozen = {
        let mut fingerprint =
            frozen_turn_fingerprint(999, "1999-01-01T00:00:00Z", None, None, &mismatch_payload);
        fingerprint["payload_bytes"] = json!(mismatch_payload.len() + 5);
        fingerprint
    };
    let victim_current = turn_fingerprint(7, "2026-11-24T17:19:31Z", None, None, &victim_payload);
    let mut stray_frozen_current = victim_current.clone();
    stray_frozen_current["frozen_payload_digest"] =
        json!(bytes_sha256_hex(victim_payload.as_bytes()));
    let open_current = turn_fingerprint(
        8,
        "2026-11-25T03:00:00Z",
        Some("conversation"),
        Some("conv-1"),
        &open_payload,
    );
    let healthy_current = turn_fingerprint(
        3,
        "2026-11-23T08:00:00Z",
        Some("conversation"),
        Some("conv-1"),
        &healthy_payload,
    );
    let mismatch_current = {
        let mut fingerprint =
            turn_fingerprint(999, "1999-01-01T00:00:00Z", None, None, &mismatch_payload);
        fingerprint["payload_bytes"] = json!(mismatch_payload.len() + 5);
        fingerprint
    };
    let stale_fence_current = {
        let mut fingerprint =
            turn_fingerprint(4, "2026-11-23T09:00:00Z", None, None, &stale_fence_payload);
        fingerprint["payload_bytes"] = json!(stale_fence_payload.len() + 3);
        fingerprint
    };
    let victim_digest = bytes_sha256_hex(victim_payload.as_bytes());
    let open_digest = bytes_sha256_hex(open_payload.as_bytes());
    let healthy_digest = bytes_sha256_hex(healthy_payload.as_bytes());

    // v119-positive and v116-positive rows: repairs must never leave this
    // scope, and the chain must keep their refreshed shapes intact.
    let legacy_work_frozen = {
        let mut fingerprint = json!({
            "content_hash": null,
            "sequence": null,
            "turn_index": null,
            "created_at": "",
            "owner_kind": null,
            "owner_id": null,
            "revision_marker": "9:7:in_flight",
            "payload_bytes": legacy_work_payload.len(),
            "related_turn_id": null,
        });
        fingerprint["frozen_payload_digest"] =
            json!(bytes_sha256_hex(legacy_work_payload.as_bytes()));
        fingerprint
    };
    let legacy_work_current = json!({
        "content_hash": null,
        "sequence": null,
        "turn_index": null,
        "created_at": "",
        "owner_kind": null,
        "owner_id": null,
        "revision_marker": "9:7:in_flight",
        "payload_bytes": legacy_work_payload.len(),
        "related_turn_id": null,
    });
    let legacy_work_digest = bytes_sha256_hex(legacy_work_payload.as_bytes());
    let outcome_payload = r#"{"outcome_id":"outcome-legacy","created_at":"2026-11-20T00:00:00Z"}"#;
    let outcome_current = json!({
        "content_hash": null,
        "sequence": null,
        "turn_index": null,
        "created_at": "2026-11-20T00:00:00Z",
        "owner_kind": null,
        "owner_id": null,
        "revision_marker": "attempt-legacy",
        "payload_bytes": outcome_payload.len(),
        "related_turn_id": null,
    });
    let outcome_digest = bytes_sha256_hex(outcome_payload.as_bytes());

    {
        let db = legacy_destructive_fence_database(&path)?;
        for (turn_id, turn_index, agent, created_at, owner_kind, owner_id, terminal, payload) in [
            (
                "turn-victim",
                7,
                "victim",
                "2026-11-24T17:19:31Z",
                None,
                None,
                Some("completed"),
                &victim_payload,
            ),
            (
                "turn-open",
                8,
                "victim",
                "2026-11-25T03:00:00Z",
                Some("conversation"),
                Some("conv-1"),
                None,
                &open_payload,
            ),
            (
                "turn-healthy",
                3,
                "healthy",
                "2026-11-23T08:00:00Z",
                Some("conversation"),
                Some("conv-1"),
                Some("completed"),
                &healthy_payload,
            ),
            (
                "turn-mismatch",
                3,
                "healthy",
                "2026-11-23T10:00:00Z",
                None,
                None,
                Some("completed"),
                &mismatch_payload,
            ),
            (
                "turn-stale-fence",
                4,
                "healthy",
                "2026-11-23T09:00:00Z",
                None,
                None,
                Some("completed"),
                &stale_fence_payload,
            ),
        ] {
            db.execute(
                "INSERT INTO turn_records
                 (turn_id, turn_index, agent_id, terminal_kind, created_at, completed_at,
                  payload_json, owner_kind, owner_id)
                 VALUES (?1, ?2, ?3, ?7, ?4, '2026-11-26T00:00:00Z', ?8, ?5, ?6)",
                params![
                    turn_id, turn_index, agent, created_at, owner_kind, owner_id, terminal, payload
                ],
            )?;
        }
        for (seed, agent) in [
            ("seed-victim", "victim"),
            ("seed-healthy", "healthy"),
            ("seed-foreign", "other"),
            ("seed-legacy-work", "legacy-work"),
        ] {
            db.execute(
                "INSERT INTO episode_continuity_seeds
                 (seed_id, agent_id, policy_version, boundary_id, source_kind, roots_json,
                  candidate_payloads_json, import_required, captured_at)
                 VALUES (?1, ?2, 1, 'boundary', 'upgrade_unbound', '{}', '[]', 0, 'a')",
                params![seed, agent],
            )?;
        }
        for (seed, phase, family, record, fingerprint) in [
            (
                "seed-victim",
                "upgrade",
                "turn",
                "turn-victim",
                &victim_frozen,
            ),
            (
                "seed-victim",
                "first_import",
                "turn",
                "turn-victim",
                &victim_frozen,
            ),
            ("seed-victim", "upgrade", "turn", "turn-open", &open_frozen),
            (
                "seed-healthy",
                "upgrade",
                "turn",
                "turn-healthy",
                &healthy_frozen,
            ),
            (
                "seed-healthy",
                "upgrade",
                "turn",
                "turn-mismatch",
                &mismatch_frozen,
            ),
            // A foreign seed never enters this agent's scope.
            (
                "seed-foreign",
                "upgrade",
                "turn",
                "turn-victim",
                &victim_frozen,
            ),
            (
                "seed-legacy-work",
                "upgrade",
                "work_execution",
                "w1",
                &legacy_work_frozen,
            ),
        ] {
            db.execute(
                "INSERT INTO input_evidence_eligibility VALUES
                 (?1, ?2, ?3, ?4, ?5, 1)",
                params![seed, phase, family, record, fingerprint.to_string()],
            )?;
        }
        for (seed, record, fingerprint) in [
            ("seed-victim", "turn-victim", &victim_frozen),
            ("seed-victim", "turn-open", &open_frozen),
            ("seed-healthy", "turn-mismatch", &mismatch_frozen),
        ] {
            db.execute(
                "INSERT INTO input_evidence_seed_recovery_current
                 (seed_id, family, record_id, fingerprint_json, revision, origin_kind)
                 VALUES (?1, 'turn', ?2, ?3, 4, 'fixed_member')",
                params![seed, record, fingerprint.to_string()],
            )?;
        }
        for (agent, incarnation, family, record, digest, fingerprint) in [
            (
                "victim",
                1,
                "turn",
                "turn-victim",
                victim_digest.as_str(),
                &victim_current,
            ),
            (
                "victim",
                2,
                "turn",
                "turn-victim",
                victim_digest.as_str(),
                &stray_frozen_current,
            ),
            (
                "victim",
                1,
                "turn",
                "turn-open",
                open_digest.as_str(),
                &open_current,
            ),
            (
                "healthy",
                1,
                "turn",
                "turn-healthy",
                healthy_digest.as_str(),
                &healthy_current,
            ),
            (
                "healthy",
                1,
                "turn",
                "turn-mismatch",
                "stale-digest",
                &mismatch_current,
            ),
            (
                "healthy",
                1,
                "turn",
                "turn-stale-fence",
                "stale-digest",
                &stale_fence_current,
            ),
            (
                "legacy-work",
                1,
                "work_execution",
                "w1",
                legacy_work_digest.as_str(),
                &legacy_work_current,
            ),
            (
                "victim",
                1,
                "outcome",
                "outcome-legacy",
                outcome_digest.as_str(),
                &outcome_current,
            ),
        ] {
            db.execute(
                "INSERT INTO input_evidence_current
                 (agent_id, identity_incarnation, family, record_id, revision, content_digest,
                  fingerprint_json, availability, qualification)
                 VALUES (?1, ?2, ?3, ?4, 5, ?5, ?6, 'present', 'committed_writer')",
                params![
                    agent,
                    incarnation,
                    family,
                    record,
                    digest,
                    fingerprint.to_string()
                ],
            )?;
        }
        db.execute(
            "INSERT INTO execution_protocol_work_items VALUES
             ('legacy-work', 'w1', 9, 7, 'in_flight', ?1)",
            [legacy_work_payload],
        )?;
    }

    let mut db = Connection::open(&path)?;
    for version in 118..=122 {
        apply_migration(&mut db, migration(version), unexpected_runtime_backfill)?;
    }
    assert_eq!(current_schema_version(&db)?, 122);

    // A payload that still carries a live fence is not a v121 rewrite shape.
    db.execute(
        "UPDATE turn_records
         SET payload_json = json_set(payload_json, '$.destructive_operation_id', 'fence-op-3')
         WHERE turn_id = 'turn-stale-fence'",
        [],
    )?;

    // The unfixed chain reproduces the production drift: the fence key is gone
    // while the frozen fingerprints keep the pre-rewrite byte length.
    let victim_payload: String = db.query_row(
        "SELECT payload_json FROM turn_records WHERE turn_id = 'turn-victim'",
        [],
        |row| row.get(0),
    )?;
    assert!(serde_json::from_str::<Value>(&victim_payload)?
        .get("destructive_operation_id")
        .is_none());
    let open_payload: String = db.query_row(
        "SELECT payload_json FROM turn_records WHERE turn_id = 'turn-open'",
        [],
        |row| row.get(0),
    )?;
    let open_value = serde_json::from_str::<Value>(&open_payload)?;
    assert!(open_value.get("destructive_operation_id").is_none());
    assert_eq!(
        open_value.get("external_side_effect_possible"),
        Some(&json!(true))
    );
    let (stored_bytes, actual_bytes): (i64, i64) = db.query_row(
        "SELECT json_extract(c.fingerprint_json, '$.payload_bytes'),
                length(CAST(r.payload_json AS BLOB))
         FROM input_evidence_current c
         JOIN turn_records r
           ON r.agent_id = c.agent_id AND r.turn_id = c.record_id
         WHERE c.agent_id = 'victim' AND c.identity_incarnation = 1
           AND c.family = 'turn' AND c.record_id = 'turn-victim'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_ne!(
        stored_bytes, actual_bytes,
        "the unfixed chain must leave the frozen Turn fingerprint drifted"
    );

    // v119 kept refreshing WorkExecution fingerprints in the same chain.
    let new_work_payload: String = db.query_row(
        "SELECT payload_json FROM execution_protocol_work_items WHERE work_item_id = 'w1'",
        [],
        |row| row.get(0),
    )?;
    let (work_marker, work_bytes, work_digest, work_current_digest): (String, i64, String, String) =
        db.query_row(
            "SELECT json_extract(e.fingerprint_json, '$.revision_marker'),
                json_extract(e.fingerprint_json, '$.payload_bytes'),
                json_extract(e.fingerprint_json, '$.frozen_payload_digest'),
                c.content_digest
         FROM input_evidence_eligibility e
         JOIN input_evidence_current c
           ON c.agent_id = 'legacy-work' AND c.family = 'work_execution' AND c.record_id = 'w1'
         WHERE e.seed_id = 'seed-legacy-work' AND e.capture_phase = 'upgrade'
           AND e.family = 'work_execution' AND e.record_id = 'w1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    assert_eq!(work_marker, "9:8:runnable");
    assert_eq!(work_bytes, new_work_payload.len() as i64);
    assert_eq!(work_digest, bytes_sha256_hex(new_work_payload.as_bytes()));
    assert_eq!(
        work_current_digest,
        bytes_sha256_hex(new_work_payload.as_bytes())
    );

    let before = snapshot_input_evidence(&db)?;
    apply_migration(&mut db, migration(123), unexpected_runtime_backfill)?;
    assert_eq!(current_schema_version(&db)?, 123);
    let after = snapshot_input_evidence(&db)?;

    assert_eq!(before.len(), after.len());
    let changed: Vec<&String> = before
        .keys()
        .filter(|key| after.get(*key).is_some_and(|value| value != &before[*key]))
        .collect();
    let mut expected_keys = vec![
        "eligibility|seed-victim|upgrade|turn|turn-victim",
        "eligibility|seed-victim|first_import|turn|turn-victim",
        "eligibility|seed-victim|upgrade|turn|turn-open",
        "seed_recovery|seed-victim|turn|turn-victim|4",
        "seed_recovery|seed-victim|turn|turn-open|4",
        "current|victim|1|turn|turn-victim|5|present",
        "current|victim|2|turn|turn-victim|5|present",
        "current|victim|1|turn|turn-open|5|present",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    expected_keys.sort();
    let mut changed_keys = changed.into_iter().cloned().collect::<Vec<_>>();
    changed_keys.sort();
    assert_eq!(
        changed_keys, expected_keys,
        "only v121-drifted Turn fingerprints may change"
    );

    // Recompute the projection exactly as the runtime's fingerprint_on does.
    let new_victim_current =
        turn_fingerprint(7, "2026-11-24T17:19:31Z", None, None, &victim_payload);
    let new_victim_frozen =
        frozen_turn_fingerprint(7, "2026-11-24T17:19:31Z", None, None, &victim_payload);
    let new_open_current = turn_fingerprint(
        8,
        "2026-11-25T03:00:00Z",
        Some("conversation"),
        Some("conv-1"),
        &open_payload,
    );
    let new_open_frozen = frozen_turn_fingerprint(
        8,
        "2026-11-25T03:00:00Z",
        Some("conversation"),
        Some("conv-1"),
        &open_payload,
    );
    let new_victim_digest = bytes_sha256_hex(victim_payload.as_bytes());
    let new_open_digest = bytes_sha256_hex(open_payload.as_bytes());

    for (agent, incarnation, record, expected, digest) in [
        (
            "victim",
            1,
            "turn-victim",
            &new_victim_current,
            &new_victim_digest,
        ),
        (
            "victim",
            2,
            "turn-victim",
            &new_victim_current,
            &new_victim_digest,
        ),
        (
            "victim",
            1,
            "turn-open",
            &new_open_current,
            &new_open_digest,
        ),
    ] {
        let (fingerprint, content_digest): (String, String) = db.query_row(
            "SELECT fingerprint_json, content_digest FROM input_evidence_current
             WHERE agent_id = ?1 AND identity_incarnation = ?2
               AND family = 'turn' AND record_id = ?3",
            params![agent, incarnation, record],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(serde_json::from_str::<Value>(&fingerprint)?, *expected);
        assert_eq!(content_digest, *digest);
    }
    for (seed, phase, record, expected) in [
        ("seed-victim", "upgrade", "turn-victim", &new_victim_frozen),
        (
            "seed-victim",
            "first_import",
            "turn-victim",
            &new_victim_frozen,
        ),
        ("seed-victim", "upgrade", "turn-open", &new_open_frozen),
    ] {
        let fingerprint: String = db.query_row(
            "SELECT fingerprint_json FROM input_evidence_eligibility
             WHERE seed_id = ?1 AND capture_phase = ?2
               AND family = 'turn' AND record_id = ?3",
            params![seed, phase, record],
            |row| row.get(0),
        )?;
        assert_eq!(serde_json::from_str::<Value>(&fingerprint)?, *expected);
    }
    // Seed membership serves the repaired fixed inputs, not only Current.
    for (seed, record, expected) in [
        ("seed-victim", "turn-victim", &new_victim_frozen),
        ("seed-victim", "turn-open", &new_open_frozen),
    ] {
        let mut statement = db.prepare(
            "SELECT fingerprint_json FROM input_evidence_seed_membership
             WHERE seed_id = ?1 AND family = 'turn' AND record_id = ?2",
        )?;
        let rows = statement
            .query_map(params![seed, record], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(!rows.is_empty(), "{seed}/{record}");
        for fingerprint in rows {
            assert_eq!(serde_json::from_str::<Value>(&fingerprint)?, *expected);
        }
        let fingerprint: String = db.query_row(
            "SELECT fingerprint_json FROM input_evidence_seed_recovery_current
             WHERE seed_id = ?1 AND family = 'turn' AND record_id = ?2",
            params![seed, record],
            |row| row.get(0),
        )?;
        assert_eq!(serde_json::from_str::<Value>(&fingerprint)?, *expected);
    }
    // The eligibility branch of the membership view stays healthy.
    let healthy_membership: String = db.query_row(
        "SELECT fingerprint_json FROM input_evidence_seed_membership
         WHERE seed_id = 'seed-healthy' AND family = 'turn' AND record_id = 'turn-healthy'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<Value>(&healthy_membership)?,
        healthy_frozen
    );

    let report: String = db.query_row(
        "SELECT value FROM runtime_metadata
         WHERE key = 'repair_retired_destructive_operation_turn_fingerprints'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<Value>(&report)?,
        json!({
            "repaired": {
                "input_evidence_current": 3,
                "input_evidence_eligibility": 3,
                "input_evidence_seed_recovery_current": 2,
            },
            "non_v121": {
                "input_evidence_current": 2,
                "input_evidence_eligibility": 1,
                "input_evidence_seed_recovery_current": 1,
            },
        })
    );

    // Re-entering the repair after a simulated lost ledger row is a no-op on
    // already-repaired rows and keeps counting the non-v121 drift.
    db.execute("DELETE FROM schema_migrations WHERE version = 123", [])?;
    apply_migration(&mut db, migration(123), unexpected_runtime_backfill)?;
    assert_eq!(snapshot_input_evidence(&db)?, after);
    let rerun_report: String = db.query_row(
        "SELECT value FROM runtime_metadata
         WHERE key = 'repair_retired_destructive_operation_turn_fingerprints'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<Value>(&rerun_report)?,
        json!({
            "repaired": {
                "input_evidence_current": 0,
                "input_evidence_eligibility": 0,
                "input_evidence_seed_recovery_current": 0,
            },
            "non_v121": {
                "input_evidence_current": 2,
                "input_evidence_eligibility": 1,
                "input_evidence_seed_recovery_current": 1,
            },
        })
    );
    Ok(())
}

#[test]
fn repair_retired_destructive_operation_turn_fingerprints_accepts_compacted_baselines() -> Result<()>
{
    let migration = MIGRATIONS
        .iter()
        .find(|migration| {
            migration.name == "repair_retired_destructive_operation_turn_fingerprints"
        })
        .unwrap();
    for has_metadata in [false, true] {
        let mut db = Connection::open_in_memory()?;
        ensure_migration_table(&db)?;
        db.execute_batch(
            "INSERT INTO schema_migrations VALUES (122, 'usage_attempts', 'fixture');",
        )?;
        if has_metadata {
            db.execute_batch(
                "CREATE TABLE runtime_metadata (
                   key TEXT PRIMARY KEY, value TEXT NOT NULL,
                   created_at TEXT NOT NULL, updated_at TEXT NOT NULL
                 );
                 INSERT INTO runtime_metadata (key, value, created_at, updated_at)
                 VALUES ('unrelated', 'preserved', 'a', 'a');",
            )?;
        }
        for _ in 0..2 {
            apply_migration(&mut db, migration, unexpected_runtime_backfill)?;
            assert_eq!(current_schema_version(&db)?, migration.version);
            let tx = db.transaction()?;
            assert!(!table_exists_tx(&tx, "turn_records")?);
            if has_metadata {
                // No Turn spine means no scan and no report row.
                let keys = tx
                    .prepare("SELECT key FROM runtime_metadata")?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(keys, ["unrelated"]);
            }
        }
    }
    Ok(())
}
