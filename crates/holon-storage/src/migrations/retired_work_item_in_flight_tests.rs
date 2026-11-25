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

fn legacy_in_flight_payload(attempt_id: &str, generation: u64) -> String {
    serde_json::json!({
        "source_revision": 9,
        "state": {"state": "in_flight", "generation": generation, "attempt_id": attempt_id},
    })
    .to_string()
}

fn legacy_attempt(
    agent: &str,
    attempt_id: &str,
    work_item_id: &str,
    state: holon_domain::execution_protocol::ExecutionAttemptState,
    identity: holon_domain::execution_protocol::ExecutionSourceIdentity,
    generation: u64,
) -> holon_domain::execution_protocol::ExecutionAttempt {
    use holon_domain::execution_protocol::{
        AdmittedFences, ExecutionAttempt, ExecutionBinding, ExecutionOrigin, ExecutionPriority,
        ExecutionProvenance, ExecutionSource, ExecutionTrust,
    };
    ExecutionAttempt {
        attempt_id: attempt_id.to_owned(),
        agent_id: agent.to_owned(),
        source_message_id: None,
        input_projection: None,
        source: ExecutionSource {
            identity,
            generation: 1,
        },
        binding: ExecutionBinding::WorkItem {
            work_item_id: work_item_id.to_owned(),
        },
        provenance: ExecutionProvenance {
            origin: ExecutionOrigin::Operator,
            trust: ExecutionTrust::OperatorInstruction,
            priority: ExecutionPriority::Normal,
            correlation_id: None,
            causation_id: None,
        },
        admitted_fences: AdmittedFences {
            source_revision: 9,
            work_item_source_revision: Some(9),
            work_item_generation: Some(generation),
            rejoin: None,
            agent_control_revision: 1,
            host_registry_revision: 1,
        },
        state,
        run_id: None,
        turn_id: None,
        recovery_of_attempt_id: None,
        terminal_outcome_id: None,
        admitted_at: "2026-11-25T00:00:00Z".to_owned(),
        terminal_at: None,
    }
}

fn open_legacy_database(path: &std::path::Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    ensure_migration_table(&db)?;
    for version in [6, 41, 113] {
        db.execute_batch(
            MIGRATIONS
                .iter()
                .find(|migration| migration.version == version)
                .unwrap()
                .sql,
        )?;
    }
    Ok(db)
}

/// The pre-migration reader must reproduce exactly what migration 119 writes,
/// because the interrupted-upgrade recovery report decodes the database with
/// the candidate binary before that migration runs.
#[test]
fn pre_migration_projection_matches_retirement_canonical_rows() -> Result<()> {
    use crate::execution::{project_legacy_in_flight_work_item, LegacyWaitSource};
    use holon_domain::execution_protocol::{
        ExecutionAttemptState, ExecutionSourceIdentity, WorkItemExecutionState,
    };

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite3");
    {
        let db = open_legacy_database(&path)?;
        for (agent, work, attempt_id) in [
            ("ordinary", "w1", "open-ordinary"),
            ("exact", "w2", "open-exact"),
            ("recovery", "w3", "open-recovery"),
            ("settled", "w4", "settled"),
            ("missing", "w5", "missing"),
        ] {
            db.execute(
                "INSERT INTO execution_protocol_work_items VALUES (?1, ?2, 9, 7, 'in_flight', ?3)",
                params![agent, work, legacy_in_flight_payload(attempt_id, 7)],
            )?;
        }
        db.execute(
            "INSERT INTO execution_protocol_work_items VALUES ('unrelated', 'w6', 9, 7, 'paused', ?1)",
            [r#"{"source_revision":9,"state":{"state":"paused","generation":7,"reason":"manual"}}"#],
        )?;
        for (agent, lifecycle, attempt) in [
            (
                "ordinary",
                "open",
                legacy_attempt(
                    "ordinary",
                    "open-ordinary",
                    "w1",
                    ExecutionAttemptState::Open,
                    ExecutionSourceIdentity::WorkItemContinuation {
                        work_item_id: "w1".into(),
                    },
                    7,
                ),
            ),
            (
                "exact",
                "open",
                legacy_attempt(
                    "exact",
                    "open-exact",
                    "w2",
                    ExecutionAttemptState::Open,
                    ExecutionSourceIdentity::TriggeredWait {
                        wait_id: "resolved".into(),
                        trigger_message_id: "m".into(),
                        ingress: None,
                    },
                    7,
                ),
            ),
            (
                "recovery",
                "open",
                legacy_attempt(
                    "recovery",
                    "open-recovery",
                    "w3",
                    ExecutionAttemptState::Open,
                    ExecutionSourceIdentity::TriggeredWait {
                        wait_id: "stale".into(),
                        trigger_message_id: "m".into(),
                        ingress: None,
                    },
                    7,
                ),
            ),
            (
                "settled",
                "settled",
                legacy_attempt(
                    "settled",
                    "settled",
                    "w4",
                    ExecutionAttemptState::Settled,
                    ExecutionSourceIdentity::WorkItemContinuation {
                        work_item_id: "w4".into(),
                    },
                    7,
                ),
            ),
        ] {
            db.execute(
                "INSERT INTO execution_protocol_attempts (
                     agent_id, attempt_id, lifecycle_state, source_identity_json,
                     source_generation, payload_json
                 ) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
                params![
                    agent,
                    attempt.attempt_id,
                    lifecycle,
                    serde_json::to_string(&attempt.source.identity)?,
                    serde_json::to_string(&attempt)?,
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
                 ('other', 'stranger', 'w1', 'active', 'timer', 'timer', 'a', 'a', '{}');",
        )?;
    }

    let projected = {
        let db = Connection::open(&path)?;
        let mut projected = std::collections::BTreeMap::new();
        for (agent, work) in [
            ("ordinary", "w1"),
            ("exact", "w2"),
            ("recovery", "w3"),
            ("settled", "w4"),
            ("missing", "w5"),
            ("unrelated", "w6"),
        ] {
            let (payload, marker): (String, String) = db.query_row(
                "SELECT payload_json, source_revision||':'||generation||':'||lifecycle_state
                 FROM execution_protocol_work_items WHERE agent_id = ?1 AND work_item_id = ?2",
                params![agent, work],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let (record, evidence) =
                project_legacy_in_flight_work_item(&db, agent, work, &payload, &marker)?;
            projected.insert(work.to_owned(), (record, evidence));
        }
        projected
    };

    // The projection itself: one case per retired-shape branch.
    let expected: [(&str, u64, &str, Option<&str>, LegacyWaitSource, bool); 5] = [
        (
            "w1",
            7,
            "runnable",
            None::<&str>,
            LegacyWaitSource::None,
            true,
        ),
        (
            "w2",
            7,
            "waiting",
            Some("resolved"),
            LegacyWaitSource::ExactAttemptWait,
            true,
        ),
        (
            "w3",
            7,
            "waiting",
            Some("active"),
            LegacyWaitSource::LatestOwnerWait,
            true,
        ),
        ("w4", 8, "runnable", None, LegacyWaitSource::None, false),
        ("w5", 8, "runnable", None, LegacyWaitSource::None, false),
    ];
    for (work, generation, lifecycle, wait_id, wait_source, matched) in expected {
        let (record, evidence) = projected.get(work).unwrap();
        assert_eq!(record.source_revision, 9, "{work}");
        assert_eq!(record.generation(), generation, "{work}");
        let evidence = evidence.as_ref().expect("legacy row must carry evidence");
        assert_eq!(evidence.original_lifecycle, "in_flight", "{work}");
        assert_eq!(evidence.matching_open_attempt, matched, "{work}");
        assert_eq!(evidence.wait_source, wait_source, "{work}");
        assert_eq!(evidence.wait_id.as_deref(), wait_id, "{work}");
        assert_eq!(evidence.generation_before, 7, "{work}");
        assert_eq!(evidence.generation_after, generation, "{work}");
        match (&record.state, lifecycle) {
            (WorkItemExecutionState::Waiting { wait, .. }, "waiting") => {
                assert_eq!(Some(wait.wait_id.as_str()), wait_id, "{work}")
            }
            (WorkItemExecutionState::Runnable { recovery_ref, .. }, "runnable") => {
                assert_eq!(recovery_ref, &None, "{work}")
            }
            (state, _) => panic!("unexpected projected state for {work}: {state:?}"),
        }
    }
    let (paused, evidence) = projected.get("w6").unwrap();
    assert!(evidence.is_none());
    assert!(matches!(
        paused.state,
        WorkItemExecutionState::Paused { ref reason, .. } if reason == "manual"
    ));

    // Canonical rows written by migration 119 must agree with the projection.
    let mut db = Connection::open(&path)?;
    apply_migration(
        &mut db,
        MIGRATIONS
            .iter()
            .find(|migration| migration.version == 119)
            .unwrap(),
        unexpected_runtime_backfill,
    )?;
    let canonical_rows: [(&str, u64, &str, Option<&str>); 6] = [
        ("w1", 7, "runnable", None),
        ("w2", 7, "waiting", Some("resolved")),
        ("w3", 7, "waiting", Some("active")),
        ("w4", 8, "runnable", None),
        ("w5", 8, "runnable", None),
        ("w6", 7, "paused", None),
    ];
    for (work, generation, lifecycle, wait_id) in canonical_rows {
        let (indexed_generation, indexed_lifecycle, payload): (u64, String, String) = db
            .query_row(
                "SELECT generation, lifecycle_state, payload_json
             FROM execution_protocol_work_items WHERE work_item_id = ?1",
                [work],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        assert_eq!(indexed_generation, generation, "{work}");
        assert_eq!(indexed_lifecycle, lifecycle, "{work}");
        let canonical: WorkItemExecutionRecord = serde_json::from_str(&payload)?;
        let (projected_record, _) = projected.get(work).unwrap();
        assert_eq!(
            projected_record, &canonical,
            "projection disagrees with migration 119 canonical row for {work}"
        );
        if let WorkItemExecutionState::Waiting { wait, .. } = &canonical.state {
            assert_eq!(Some(wait.wait_id.as_str()), wait_id, "{work}");
        }
    }
    Ok(())
}

/// Anything the projection cannot reconcile must fail the agent's report
/// instead of being silently legalized.
#[test]
fn pre_migration_projection_fails_closed_on_damaged_legacy_rows() -> Result<()> {
    use crate::execution::project_legacy_in_flight_work_item;

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.sqlite3");
    {
        let db = open_legacy_database(&path)?;
        for (agent, work, generation, payload) in [
            ("malformed", "w1", 7, "not json".to_owned()),
            (
                "unknown",
                "w2",
                7,
                r#"{"source_revision":9,"state":{"state":"mystery","generation":7,"attempt_id":"a"}}"#
                    .to_owned(),
            ),
            (
                "missing-key",
                "w3",
                7,
                r#"{"source_revision":9,"state":{"state":"in_flight","generation":7}}"#.to_owned(),
            ),
            (
                "drifted",
                "w4",
                7,
                r#"{"source_revision":9,"state":{"state":"in_flight","generation":8,"attempt_id":"a"}}"#
                    .to_owned(),
            ),
            ("overflow", "w5", i64::MAX, legacy_in_flight_payload("a", i64::MAX as u64)),
            (
                "damaged-attempt",
                "w6",
                7,
                legacy_in_flight_payload("broken", 7),
            ),
        ] {
            db.execute(
                "INSERT INTO execution_protocol_work_items VALUES (?1, ?2, 9, ?3, 'in_flight', ?4)",
                params![agent, work, generation, payload],
            )?;
        }
        db.execute(
            "INSERT INTO execution_protocol_attempts (
                 agent_id, attempt_id, lifecycle_state, source_identity_json,
                 source_generation, payload_json
             ) VALUES ('damaged-attempt', 'broken', 'open', '{}', 1, '{}')",
            [],
        )?;
    }

    let db = Connection::open(&path)?;
    for (agent, work, marker) in [
        ("malformed", "w1", "9:7:in_flight"),
        ("unknown", "w2", "9:7:in_flight"),
        ("missing-key", "w3", "9:7:in_flight"),
        ("drifted", "w4", "9:7:in_flight"),
        ("overflow", "w5", "9:9223372036854775807:in_flight"),
        ("damaged-attempt", "w6", "9:7:in_flight"),
        ("malformed", "w1", "not-a-marker"),
    ] {
        let payload: String = db.query_row(
            "SELECT payload_json FROM execution_protocol_work_items
             WHERE agent_id = ?1 AND work_item_id = ?2",
            params![agent, work],
            |row| row.get(0),
        )?;
        let error = project_legacy_in_flight_work_item(&db, agent, work, &payload, marker)
            .expect_err("damaged legacy row must fail closed");
        assert!(
            !format!("{error:#}").is_empty(),
            "{agent}/{work} must explain the failure"
        );
    }
    Ok(())
}
