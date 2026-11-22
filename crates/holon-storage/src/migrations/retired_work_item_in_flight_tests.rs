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
