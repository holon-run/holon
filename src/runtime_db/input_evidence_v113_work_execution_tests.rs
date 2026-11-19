use super::*;
use crate::domain::execution_protocol::{WorkItemExecutionRecord, WorkItemExecutionState};

const LEGACY: &str =
    r#"{"source_revision":7,"state":{"state":"needs_repair","generation":3,"repair_id":"repair"}}"#;

#[test]
fn v113_work_execution_keeps_scoped_proofs_and_the_pause_across_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let lock = directory.path().join("runtime.lock");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 112) {
        apply_migration(&mut connection, migration)?;
    }
    let scope = scope();
    let tx = connection.transaction()?;
    upsert_agent_identity_tx(&tx, &identity())?;
    upsert_agent_state_tx(&tx, &AgentState::new("agent-a"))?;
    tx.execute_batch(
        "INSERT INTO execution_protocol_partitions VALUES('agent-a','2026-10-12');
         INSERT INTO episode_continuity_seeds(
           seed_id,agent_id,policy_version,boundary_id,identity_incarnation,
           source_kind,roots_json,candidate_payloads_json,import_required,captured_at
         ) VALUES ('seed-a','agent-a',1,'fixture',1,'upgrade_identity','{}','[]',0,'2026-10-12');",
    )?;
    // Identity upsert installs a Fresh binding; this fixture represents an upgraded identity.
    assert_eq!(
        tx.execute(
            "UPDATE agent_episode_continuity
             SET mode='legacy_adopted',seed_id='seed-a',bound_at='2026-10-12'
             WHERE agent_id='agent-a' AND identity_incarnation=1 AND policy_version=1
               AND mode='fresh_empty'",
            [],
        )?,
        1
    );
    let digest = bytes_digest(LEGACY.as_bytes());
    for id in ["alias", "fixed", "overlay"] {
        let key = Key::new(Family::WorkExecution, id);
        tx.execute(
            "INSERT INTO execution_protocol_work_items VALUES('agent-a',?1,7,3,'needs_repair',?2)",
            params![id, LEGACY],
        )?;
        let mut before = fingerprint_on(&tx, "agent-a", &key)?.unwrap();
        assert_eq!(before.revision_marker, "7:3:needs_repair");
        if id == "alias" {
            upsert_alias_tx(
                &tx,
                &scope,
                &key,
                &before,
                Some(&digest),
                true,
                None,
                "committed_writer",
                None,
            )?;
        } else {
            before.frozen_payload_digest = Some(digest.clone());
            let proof = serde_json::to_string(&before)?;
            tx.execute(
                "INSERT INTO input_evidence_eligibility VALUES('seed-a','upgrade','work_execution',?1,?2,1)",
                params![id, proof],
            )?;
            if id == "overlay" {
                tx.execute(
                    "INSERT INTO input_evidence_seed_recovery_current
                     (seed_id,family,record_id,revision,fingerprint_json,origin_kind)
                     VALUES('seed-a','work_execution',?1,1,?2,'fixed_member')",
                    params![id, proof],
                )?;
                // Only the overlay may validate this selected row.
                tx.execute(
                    "UPDATE input_evidence_eligibility SET fingerprint_json='{}' WHERE record_id='overlay'",
                    [],
                )?;
            }
        }
    }
    tx.commit()?;
    drop(connection);

    let expected = WorkItemExecutionRecord {
        source_revision: 7,
        state: WorkItemExecutionState::Paused {
            generation: 3,
            reason: "repair".into(),
        },
    };
    // First open actually runs v113; second opens the already-migrated DB.
    for reopened in [false, true] {
        let db = RuntimeDb::open_and_migrate(&path, &lock)?;
        let records = db.transitions().observer_live_work_execution("agent-a")?;
        db.transaction(|tx| {
            let binding = binding_on(tx, &scope)?;
            for id in ["alias", "fixed", "overlay"] {
                let key = Key::new(Family::WorkExecution, id);
                let fingerprint = fingerprint_on(tx, "agent-a", &key)?.unwrap();
                assert_eq!(fingerprint.revision_marker, "7:3:paused");
                assert_eq!(records[id], expected);
                let observed =
                    observe_input_qualification_tx(tx, &scope, &binding, &key, fingerprint, true)?;
                match observed.canonical.unwrap() {
                    CanonicalInputEvidence::WorkExecution {
                        work_item_id,
                        record,
                    } => {
                        assert_eq!(work_item_id, id);
                        assert_eq!(record, expected);
                    }
                    _ => panic!("expected WorkExecution"),
                }
                assert_eq!(observed.digest, digest);
                assert_eq!(payload_on(tx, "agent-a", &key)?.as_deref(), Some(LEGACY));
                if id == "alias" {
                    assert!(!observed.needs_qualification);
                    let alias = alias_on(tx, &scope, &key)?.unwrap();
                    assert_eq!(alias.fingerprint.revision_marker, "7:3:needs_repair");
                    assert_eq!(alias.revision, 1);
                    let other = InputEvidenceScope {
                        identity_incarnation: 2,
                        ..scope.clone()
                    };
                    assert!(alias_on(tx, &other, &key)?.is_none());
                }
            }
            if reopened {
                let mut request = request(InputWindowSelector::RecentBriefs, 0);
                request.windows.clear();
                request.mandatory_roots.extend(
                    ["alias", "fixed", "overlay"].map(|id| Key::new(Family::WorkExecution, id)),
                );
                let input = current(prepare_input_evidence_snapshot_tx(tx, &scope, &request)?)
                    .resident
                    .capture()?;
                for key in request.mandatory_roots {
                    assert!(input.lookup(&key)?.is_some(), "missing {}", key.id);
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn v113_work_execution_rejects_extra_drift_and_current_bodies_with_old_proofs() -> Result<()> {
    let (_directory, db) = fresh()?;
    db.transaction(|tx| {
        let scope = scope();
        let key = Key::new(Family::WorkExecution, "work");
        let binding = Binding { seed_id: None };
        let current_body = LEGACY.replace("needs_repair", "paused").replace("repair_id", "reason");
        for (body, marker, bad_digest) in [
            (current_body.as_str(), "7:3:needs_repair", false),
            (LEGACY, "8:3:needs_repair", false),
            (LEGACY, "7:2:needs_repair", false),
            (LEGACY, "7:3:runnable", false),
            (LEGACY, "7:3:needs_repair", true),
        ] {
            tx.execute(
                "INSERT INTO execution_protocol_work_items VALUES('agent-a','work',7,3,'paused',?1)
                 ON CONFLICT(agent_id,work_item_id) DO UPDATE SET payload_json=excluded.payload_json",
                [body],
            )?;
            let fingerprint = fingerprint_on(tx, "agent-a", &key)?.unwrap();
            let mut frozen = fingerprint.clone();
            frozen.revision_marker = marker.into();
            let digest = bytes_digest(if bad_digest { b"changed" } else { body.as_bytes() });
            upsert_alias_tx(
                tx, &scope, &key, &frozen, Some(&digest), true, None, "committed_writer", None,
            )?;
            let result = observe_input_qualification_tx(tx, &scope, &binding, &key, fingerprint, false);
            assert!(matches!(
                result.err().and_then(|error| error.downcast::<InputEvidenceSnapshotError>().ok()),
                Some(InputEvidenceSnapshotError::ProjectionChanged(_))
            ), "{marker}, {body}, {bad_digest}");
        }
        tx.execute(
            "UPDATE execution_protocol_work_items SET payload_json=?1 WHERE work_item_id='work'",
            [&current_body],
        )?;
        let mut fingerprint = fingerprint_on(tx, "agent-a", &key)?.unwrap();
        let digest = bytes_digest(current_body.as_bytes());
        upsert_alias_tx(
            tx, &scope, &key, &fingerprint, Some(&digest), true, None, "committed_writer", None,
        )?;
        observe_input_qualification_tx(tx, &scope, &binding, &key, fingerprint.clone(), false)?;
        fingerprint.payload_bytes += 1;
        assert!(observe_input_qualification_tx(tx, &scope, &binding, &key, fingerprint, false).is_err());
        Ok(())
    })
}
