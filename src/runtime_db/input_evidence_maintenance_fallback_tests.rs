use super::*;
use crate::{
    domain::execution_protocol::*,
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        repositories::{upsert_queue_entry_tx, upsert_turn_record_tx},
        transitions::execution_protocol_repository::persist_state_tx,
    },
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentState, AgentStatus, AgentVisibility,
        AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, QueueEntryRecord,
        QueueEntryStatus,
    },
};
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    }
}
fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        "agent-a",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}
fn seed_canonical(tx: &Transaction<'_>) -> Result<(MessageEnvelope, ExecutionAttempt, TurnRecord)> {
    let mut message = MessageEnvelope::new(
        "agent-a",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original operator input".into(),
        },
    );
    message.id = "source-a".into();
    message.turn_id = Some("turn-a".into());
    let message = append_message_tx(tx, &message)?.0;
    let mut state = AgentState::new("agent-a");
    state.status = AgentStatus::AwakeRunning;
    state.current_run_id = Some("run-a".into());
    state.turn_index = 1;
    state.current_turn_id = Some("turn-a".into());
    upsert_agent_state_tx(tx, &state)?;
    let mut turn = TurnRecord::new("agent-a", "turn-a", 1);
    turn.input_message_ids.push(message.id.clone());
    upsert_turn_record_tx(tx, &turn)?;
    upsert_queue_entry_tx(
        tx,
        &QueueEntryRecord {
            message_id: message.id.clone(),
            agent_id: "agent-a".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Dequeued,
            created_at: message.created_at,
            updated_at: message.created_at,
        },
    )?;
    let attempt = ExecutionAttempt {
        input_projection: None,
        attempt_id: "attempt-a".into(),
        agent_id: "agent-a".into(),
        source_message_id: Some(message.id.clone()),
        source: ExecutionSource {
            identity: ExecutionSourceIdentity::QueueMessage {
                message_id: message.id.clone(),
            },
            generation: message.message_seq.unwrap(),
        },
        binding: ExecutionBinding::Command,
        provenance: ExecutionProvenance {
            origin: ExecutionOrigin::Operator,
            trust: ExecutionTrust::OperatorInstruction,
            priority: ExecutionPriority::Normal,
            correlation_id: None,
            causation_id: None,
        },
        admitted_fences: AdmittedFences {
            source_revision: message.message_seq.unwrap(),
            work_item_source_revision: None,
            work_item_generation: None,
            rejoin: None,
            agent_control_revision: 1,
            host_registry_revision: 1,
        },
        state: ExecutionAttemptState::Open,
        run_id: None,
        turn_id: Some("turn-a".into()),
        recovery_of_attempt_id: None,
        terminal_outcome_id: None,
        admitted_at: message.created_at.to_rfc3339(),
        terminal_at: None,
    };
    let mut protocol = ExecutionProtocolState::empty("agent-a");
    protocol
        .attempts
        .insert(attempt.attempt_id.clone(), attempt.clone());
    persist_state_tx(tx, &protocol)?;
    Ok((message, attempt, turn))
}
fn current_fixture(qualified: bool) -> Result<(tempfile::TempDir, RuntimeDb, MessageEnvelope)> {
    let directory = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )?;
    let message = db.transaction(|tx| {
        upsert_agent_identity_tx(tx, &identity())?;
        let (message, attempt, turn) = seed_canonical(tx)?;
        if qualified {
            register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[
                    CanonicalInputEvidence::Message(message.clone()),
                    CanonicalInputEvidence::Turn(turn),
                    CanonicalInputEvidence::Attempt(attempt),
                ],
            )?;
        }
        Ok(message)
    })?;
    Ok((directory, db, message))
}
fn apply(
    db: &RuntimeDb,
    directory: &std::path::Path,
) -> Result<crate::runtime_db::RetiredSchedulerFallbackResult> {
    let path = directory.join("runtime-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&path)?;
    let authority = OfflineInputMaintenance::for_database(db, &lease, &path)?;
    Ok(db
        .apply_retired_scheduler_cleanup_fallbacks_with_maintenance(
            &["agent-a".into()],
            &authority,
        )?
        .pop()
        .unwrap())
}
#[test]
fn qualified_command_fallback_keeps_quarantine_semantics_and_exact_input_delta() -> Result<()> {
    let (directory, db, original) = current_fixture(true)?;
    let result = apply(&db, directory.path())?;
    let recovery = result.recovery_message_id.unwrap();
    db.transaction(|tx| {
        for key in [
            Key::new(Family::Message, "source-a"),
            Key::new(Family::Turn, "turn-a"),
            Key::new(Family::Attempt, "attempt-a"),
            Key::new(Family::Outcome, "retired-scheduler-cleanup:attempt-a"),
            Key::new(Family::Message, &recovery),
        ] {
            validate_existing_input_evidence_tx(tx, &scope(), &key)?;
        }
        let stored = decode_canonical(
            &Key::new(Family::Message, "source-a"),
            &payload_on(tx, "agent-a", &Key::new(Family::Message, "source-a"))?.unwrap(),
        )?;
        let CanonicalInputEvidence::Message(stored) = stored else {
            panic!()
        };
        assert_eq!(stored.body, original.body);
        let payload = payload_on(
            tx,
            "agent-a",
            &Key::new(Family::Outcome, "retired-scheduler-cleanup:attempt-a"),
        )?
        .unwrap();
        let outcome: ExecutionOutcomeRecord = serde_json::from_str(&payload)?;
        assert!(matches!(
            outcome.outcome,
            ExecutionOutcome::Command(CommandResult::Quarantined { .. })
        ));
        Ok(())
    })?;
    assert!(db.retired_scheduler_cleanup_inventory()?.is_fixed_point());
    assert!(apply(&db, directory.path())?.actions.is_empty());
    Ok(())
}
#[test]
fn qualified_fallback_without_real_maintenance_capability_is_not_a_bypass() -> Result<()> {
    let (_directory, db, _) = current_fixture(true)?;
    let before = db.retired_scheduler_cleanup_inventory()?;
    assert!(db
        .apply_retired_scheduler_cleanup_fallback("agent-a")
        .unwrap_err()
        .downcast_ref::<NeedsOfflineInputMaintenance>()
        .is_some());
    assert_eq!(db.retired_scheduler_cleanup_inventory()?, before);
    Ok(())
}
#[test]
fn fresh_fallback_grants_only_its_actual_new_message_not_old_history() -> Result<()> {
    let (directory, db, _) = current_fixture(false)?;
    let result = apply(&db, directory.path())?;
    db.transaction(|tx| {
        assert!(existing_input_evidence_member_tx(
            tx,
            &scope(),
            &Key::new(
                Family::Message,
                result.recovery_message_id.as_ref().unwrap()
            )
        )?);
        for key in [
            Key::new(Family::Message, "source-a"),
            Key::new(Family::Turn, "turn-a"),
            Key::new(Family::Attempt, "attempt-a"),
            Key::new(Family::Outcome, "retired-scheduler-cleanup:attempt-a"),
        ] {
            assert!(!existing_input_evidence_member_tx(tx, &scope(), &key)?);
        }
        Ok(())
    })?;
    Ok(())
}
#[test]
fn fallback_alias_failure_rolls_back_protocol_queue_new_message_and_audit() -> Result<()> {
    let (directory, db, _) = current_fixture(true)?;
    let before = db.retired_scheduler_cleanup_inventory()?;
    let counts = || {
        db.transaction(|tx| {
            let m: i64 = tx.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?;
            let a: i64 = tx.query_row("SELECT count(*) FROM audit_events", [], |r| r.get(0))?;
            let o: i64 = tx.query_row(
                "SELECT count(*) FROM execution_protocol_outcomes",
                [],
                |r| r.get(0),
            )?;
            Ok((m, a, o))
        })
    };
    let originals = counts()?;
    db.transaction(|tx| {tx.execute_batch("CREATE TRIGGER fail_fallback_alias BEFORE INSERT ON input_evidence_current WHEN NEW.family='outcome' BEGIN SELECT RAISE(ABORT,'fallback alias fault'); END")?;Ok(())})?;
    assert!(apply(&db, directory.path())
        .unwrap_err()
        .to_string()
        .contains("fallback alias fault"));
    assert_eq!(db.retired_scheduler_cleanup_inventory()?, before);
    assert_eq!(counts()?, originals);
    db.transaction(|tx| {
        validate_existing_input_evidence_tx(tx, &scope(), &Key::new(Family::Attempt, "attempt-a"))
    })?;
    Ok(())
}
fn unbound_fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let mut conn = rusqlite::Connection::open(&path)?;
    ensure_migration_table(&conn)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut conn, migration)?;
    }
    {
        let tx = conn.transaction()?;
        seed_canonical(&tx)?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db','2026-10-12T00:00:00Z')",[])?;
        tx.commit()?;
    }
    drop(conn);
    let db = RuntimeDb::open_and_migrate(&path, directory.path().join("runtime.lock"))?;
    Ok((directory, db))
}
#[test]
fn fixed_unbound_fallback_preserves_seed_and_qualifies_exact_derived_records_after_binding(
) -> Result<()> {
    let (directory, db) = unbound_fixture()?;
    let frozen = || {
        db.transaction(|tx| {let mut statement=tx.prepare("SELECT seed_id,family,record_id,fingerprint_json FROM input_evidence_eligibility ORDER BY seed_id,family,record_id,capture_phase")?;let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)})
    };
    let before = frozen()?;
    let result = apply(&db, directory.path())?;
    let recovery = result.recovery_message_id.unwrap();
    assert_eq!(frozen()?, before);
    db.transaction(|tx| {
        let seed = legacy_recovery::legacy_seed_recovery_scope_tx(tx, "agent-a")?;
        for key in [
            Key::new(Family::Outcome, "retired-scheduler-cleanup:attempt-a"),
            Key::new(Family::Message, &recovery),
        ] {
            validate_origin_tx(tx, &seed.seed_id, &key)?;
        }
        let identity = identity();
        assert!(
            crate::runtime_db::episode_snapshot::bind_unbound_episode_continuity_tx(
                tx,
                &identity,
                Utc::now()
            )?
        );
        upsert_agent_identity_tx(tx, &identity)?;
        let request = InputEvidenceLoadRequest {
            query: None,
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: vec![],
            mandatory_roots: [
                Key::new(Family::Attempt, "attempt-a"),
                Key::new(Family::Message, &recovery),
                Key::new(Family::Outcome, "retired-scheduler-cleanup:attempt-a"),
            ]
            .into_iter()
            .collect(),
            record_limit: 512,
            byte_limit: 1024 * 1024,
        };
        let InputEvidenceBootstrapSnapshot::Current(snapshot) =
            prepare_input_evidence_snapshot_tx(tx, &scope(), &request)?
        else {
            panic!("bound fallback scope is not Current")
        };
        let captured = snapshot.resident.capture()?;
        let source = captured.execution.source(&recovery)?;
        assert!(source.attempts.is_empty());
        assert_eq!(source.allocation_highwater, 0);
        Ok(())
    })?;
    Ok(())
}
#[test]
fn forged_unbound_fallback_origin_cannot_name_unrelated_legacy_roots() -> Result<()> {
    let (directory, db) = unbound_fixture()?;
    let recovery = apply(&db, directory.path())?.recovery_message_id.unwrap();
    db.transaction(|tx| {
        let seed=legacy_recovery::legacy_seed_recovery_scope_tx(tx,"agent-a")?;
        let key=Key::new(Family::Message,&recovery);
        let json:String=tx.query_row("SELECT maintenance_proof_json FROM input_evidence_seed_recovery_current WHERE seed_id=?1 AND family='message' AND record_id=?2",params![seed.seed_id,recovery],|r|r.get(0))?;
        let mut origin:FallbackOrigin=serde_json::from_str(&json)?;origin.fixed_roots.insert(Key::new(Family::Message,"unrelated-late"));
        tx.execute("UPDATE input_evidence_seed_recovery_current SET maintenance_proof_json=?1 WHERE seed_id=?2 AND family='message' AND record_id=?3",params![serde_json::to_string(&origin)?,seed.seed_id,recovery])?;
        assert!(legacy_recovery::validate_seed_overlay_on(tx,&seed.seed_id,&key).is_err());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn fallback_preserves_qualified_unchanged_legacy_attempt_bytes() -> Result<()> {
    unchanged_legacy_attempt_bytes(false)
}

#[test]
fn typed_interrupt_preserves_qualified_unchanged_legacy_attempt_bytes() -> Result<()> {
    unchanged_legacy_attempt_bytes(true)
}

fn unchanged_legacy_attempt_bytes(typed_interrupt: bool) -> Result<()> {
    use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let mut connection = rusqlite::Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 89) {
        apply_migration(&mut connection, migration)?;
    }
    let expected_payload = {
        let tx = connection.transaction()?;
        upsert_agent_identity_tx(&tx, &identity())?;
        let (_, original, _) = seed_canonical(&tx)?;
        let mut settled = original.clone();
        settled.attempt_id = "attempt-b-settled".into();
        settled.state = ExecutionAttemptState::Settled;
        settled.admitted_at = "2026-10-01T00:00:00Z".into();
        settled.terminal_at = Some("2026-10-01T00:01:00Z".into());
        settled.terminal_outcome_id = Some("outcome-b-settled".into());
        let outcome = ExecutionOutcomeRecord {
            outcome_id: "outcome-b-settled".into(),
            attempt_id: settled.attempt_id.clone(),
            outcome: ExecutionOutcome::Command(CommandResult::Applied { references: vec![] }),
            created_at: settled.terminal_at.clone().unwrap(),
        };
        let mut protocol =
            crate::runtime_db::transitions::execution_protocol_repository::load_state_unchecked_tx(
                &tx, "agent-a",
            )?;
        // The live recovery attempt roots the settled predecessor at cutover.
        protocol
            .attempts
            .get_mut(&original.attempt_id)
            .unwrap()
            .recovery_of_attempt_id = Some(settled.attempt_id.clone());
        protocol
            .attempts
            .insert(settled.attempt_id.clone(), settled.clone());
        protocol
            .outcomes
            .insert(outcome.outcome_id.clone(), outcome);
        persist_state_tx(&tx, &protocol)?;
        let mut value = serde_json::to_value(&settled)?;
        let object = value.as_object_mut().unwrap();
        object.remove("run_id");
        object.remove("recovery_of_attempt_id");
        let legacy_payload = serde_json::to_string_pretty(&value)?;
        assert_eq!(
            serde_json::from_str::<ExecutionAttempt>(&legacy_payload)?,
            settled
        );
        assert_ne!(legacy_payload, serde_json::to_string(&settled)?);
        tx.execute(
            "UPDATE execution_protocol_attempts SET payload_json=?1 WHERE agent_id='agent-a' AND attempt_id='attempt-b-settled'",
            [&legacy_payload],
        )?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db','2026-10-12T00:00:00Z')",[])?;
        tx.commit()?;
        legacy_payload
    };
    drop(connection);
    let db = RuntimeDb::open_and_migrate(&path, directory.path().join("runtime.lock"))?;
    let key = Key::new(Family::Attempt, "attempt-b-settled");
    let request = InputEvidenceLoadRequest {
        query: None,
        selected_completion_brief_ids: Default::default(),
        interrupted_sources: Default::default(),
        windows: vec![],
        mandatory_roots: [key.clone()].into_iter().collect(),
        record_limit: 512,
        byte_limit: 1024 * 1024,
    };
    let capture = || -> Result<_> {
        db.transaction(|tx| {
            let InputEvidenceBootstrapSnapshot::Current(snapshot) =
                prepare_input_evidence_snapshot_tx(tx, &scope(), &request)?
            else {
                panic!("legacy original scope must be Current")
            };
            let captured = snapshot.resident.capture()?;
            let locator = captured.lookup(&key)?.unwrap().locator.clone();
            Ok((payload_on(tx, "agent-a", &key)?.unwrap(), locator))
        })
    };
    let before = capture()?;
    assert_eq!(before.0, expected_payload);
    if typed_interrupt {
        db.transaction(|tx| {
            use crate::runtime_db::transitions::execution_protocol_repository as protocol;
            let command = ExecutionProtocolCommand::Interrupt(InterruptExecution {
                attempt_id: "attempt-a".into(),
                outcome_id: "typed-interrupt-a".into(),
                reason: "typed exact-footprint regression".into(),
                interrupted_at: Utc::now().to_rfc3339(),
            });
            let prepared = protocol::validate_execution_commands_tx(
                tx,
                "agent-a",
                None,
                &[command],
                &[],
                &[],
                &[],
            )?;
            let (actual, delta) = protocol::persist_execution_commands_with_input_facts_tx(
                tx,
                prepared,
                Some(&scope()),
            )?;
            assert_eq!(
                actual.unwrap().attempts["attempt-a"].state,
                ExecutionAttemptState::Interrupted
            );
            assert!(delta
                .records
                .iter()
                .any(|record| record.locator.key == Key::new(Family::Attempt, "attempt-a")));
            assert!(!delta.records.iter().any(|record| record.locator.key == key));
            Ok(())
        })?;
    } else {
        apply(&db, directory.path())?;
    }
    let after = capture()?;
    // Rewriting legal pretty/default encodings would invalidate the frozen
    // fingerprint or current digest even though this row was not repaired.
    assert_eq!(after, before);
    db.transaction(|tx| validate_existing_input_evidence_tx(tx, &scope(), &key))?;
    Ok(())
}
