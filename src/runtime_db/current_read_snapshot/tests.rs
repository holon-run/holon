use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        migrations::{apply_migration, ensure_migration_table, MIGRATIONS},
        RuntimeDb,
    },
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility, AuthorityClass,
        MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, QueueEntryStatus,
    },
};
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "current-read-agent".into(),
        identity_incarnation: 1,
    }
}
fn request() -> CurrentReadRequest {
    CurrentReadRequest::current(CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS)
}
fn at() -> chrono::DateTime<chrono::Utc> {
    "2026-10-01T00:00:00.123456789Z".parse().unwrap()
}
fn legacy() -> Result<(tempfile::TempDir, RuntimeDb, AgentState)> {
    legacy_events(1)
}
fn legacy_events(event_count: usize) -> Result<(tempfile::TempDir, RuntimeDb, AgentState)> {
    legacy_events_with_event_source(event_count, true)
}
fn legacy_events_with_event_source(
    event_count: usize,
    event_source_present: bool,
) -> Result<(tempfile::TempDir, RuntimeDb, AgentState)> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("runtime.sqlite");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
        apply_migration(&mut connection, migration)?;
    }
    // The manual v81 schema path must install RuntimeDb's original epoch too.
    super::super::ensure_runtime_identity_metadata(&connection)?;
    let tx = connection.transaction()?;
    let identity = AgentIdentityRecord::new(
        scope().agent_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    upsert_agent_identity_tx(&tx, &identity)?;
    let state = AgentState::new(&scope().agent_id);
    upsert_agent_state_tx(&tx, &state)?;
    tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db',?1)",[repositories::timestamp(at())])?;
    let mut message = MessageEnvelope::new(
        scope().agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "original accepted fixture".into(),
        },
    );
    message.id = "original-current-read-input".into();
    message.created_at = at();
    append_message_tx(&tx, &message)?;
    repositories::upsert_queue_entry_tx(
        &tx,
        &QueueEntryRecord {
            message_id: message.id,
            agent_id: scope().agent_id,
            priority: Priority::Next,
            status: QueueEntryStatus::Queued,
            created_at: at(),
            updated_at: at(),
        },
    )?;
    for index in 0..event_count {
        let mut event = AuditEvent::legacy(
            "original_current_read",
            serde_json::json!({"original":true,"index":index,"values":(0..128).collect::<Vec<_>>()}),
        );
        event.id = if index == 0 {
            "original-current-read-event".into()
        } else {
            format!("original-current-read-event-{index}")
        };
        event.created_at = at() + chrono::Duration::milliseconds(index as i64);
        super::super::evidence::append_audit_event_tx(&tx, Some(&scope().agent_id), &event)?;
    }
    if !event_source_present {
        // Before original v81 cutover: this family really was unavailable.
        tx.execute_batch("DROP TABLE audit_events")?;
    }
    tx.commit()?;
    drop(connection);
    let db = RuntimeDb::open_and_migrate(path, home.path().join("runtime.lock"))?;
    Ok((home, db, state))
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
#[test]
fn fixed_current_queue_and_events_capture_without_body_tables_and_release_selected_arcs(
) -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    let snapshot = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    assert_eq!(snapshot.control_revision, 1);
    let capture = snapshot.resident.capture(&request())?;
    assert!(capture.has_queued());
    assert!(!capture.has_interrupted());
    assert_eq!(capture.events.len(), 1);
    let event = Arc::downgrade(&capture.events[0]);
    let raw_key = capture.events[0].locator.key.clone();
    db.transaction(|tx|{tx.execute_batch("ALTER TABLE audit_events RENAME TO current_read_events_forbidden; ALTER TABLE queue_entries RENAME TO current_read_queue_forbidden; ALTER TABLE agent_states RENAME TO current_read_state_forbidden; ALTER TABLE agent_identities RENAME TO current_read_identity_forbidden")?;Ok(())})?;
    let again = snapshot.resident.capture(&request())?;
    assert_eq!(again.events[0].locator.key, raw_key);
    assert!(Arc::ptr_eq(&again.events[0], &capture.events[0]));
    drop((again, snapshot));
    assert!(event.upgrade().is_some());
    drop(capture);
    assert!(event.upgrade().is_none());
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn original_source_caps_once_finalizer_and_rollback_keep_queue_event_sequence_and_qualification(
) -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        Ok(())
    })?;
    let snapshot = || -> Result<(u64, u64, u64, u64)> {
        Ok(db.connection()?.query_row(
        "SELECT (SELECT revision FROM current_read_current),(SELECT COUNT(*) FROM current_read_members),
         (SELECT COUNT(*) FROM audit_events),(SELECT MAX(event_seq) FROM audit_events)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
    };
    let before = snapshot()?;
    let failed: Result<()> = db.transaction(|tx| {
        let key = CurrentReadKey::queue("original-current-read-input");
        let mut event =
            AuditEvent::legacy("actual_current_read", serde_json::json!({"frozen":true}));
        event.id = "actual-current-read-event".into();
        event.created_at = at();
        let keys = BTreeSet::from([key, CurrentReadKey::event(&event.id)]);
        let mut proof =
            prepare_current_read_write_tx(tx, &scope(), &state, &keys, &request(), &budget)?;
        let queue = QueueEntryRecord {
            message_id: "original-current-read-input".into(),
            agent_id: scope().agent_id,
            priority: Priority::Next,
            status: QueueEntryStatus::Processed,
            created_at: at(),
            updated_at: at() + chrono::Duration::seconds(1),
        };
        let queue_ack = upsert_current_read_queue_tx(&mut proof, &queue)?;
        let (actual, event_ack) = append_current_read_event_tx(&mut proof, &event)?;
        assert!(actual.event_seq > before.3);
        let committed = register_current_read_writes_tx(proof, &[queue_ack, event_ack], &state)?;
        assert_eq!(committed.base_revision, before.0);
        assert_eq!(committed.revision, before.0 + 1);
        assert!(!committed
            .snapshot
            .resident
            .capture(&request())?
            .has_queued());
        anyhow::bail!("fixture failure after actual finalizer before outer receipt")
    });
    assert!(failed.is_err());
    assert_eq!(snapshot()?, before);
    assert_eq!(
        db.queue_entries()
            .latest("original-current-read-input")?
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn raw_late_queue_never_gains_complete_metadata_and_old_scope_never_reads_successor_state(
) -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        repositories::upsert_queue_entry_tx(
            tx,
            &QueueEntryRecord {
                message_id: "late-raw-input".into(),
                agent_id: scope().agent_id,
                priority: Priority::Next,
                status: QueueEntryStatus::Queued,
                created_at: at(),
                updated_at: at(),
            },
        )?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::UnknownNamespace(CurrentReadFamily::Queue))
        ),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    db.transaction(|tx|{tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&scope().agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='{poison successor State' WHERE agent_id=?1",[&scope().agent_id])?;Ok(())})?;
    let error = db
        .transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::RetiredScope)
        ),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn saved_coverage_native_gate_precedes_corrupt_revision_decode_and_preserves_original_sidecars(
) -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        Ok(())
    })?;
    let original_count: u64 =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| {
                r.get(0)
            })?;
    let repeated = vec![
        serde_json::json!({"key":{"family":"event","id":"original-current-read-event"},"availability":"present"});
        5000
    ];
    db.transaction(|tx|{tx.execute("UPDATE current_read_current SET revision='corrupt',coverage_json=json_set(coverage_json,'$.observations',json(?1))",[serde_json::to_string(&repeated)?])?;Ok(())})?;
    let mut small = request();
    small.record_limit = 4096;
    let error = db
        .transaction(|tx| prepare_current_read_snapshot_tx(tx, &scope(), &state, &small, &budget))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, u64>(0))?,
        original_count
    );
    Ok(())
}

#[test]
fn actual_consumer_windows_keep_100_count_headers_and_64_body_arcs_until_explicit_expansion(
) -> Result<()> {
    let (_home, db, state) = legacy_events(100)?;
    let budget = ledger()?;
    let original = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    assert_eq!(
        original.resident.coverage().events.len(),
        SUMMARY_EVENT_COUNT_WINDOW
    );
    assert_eq!(
        original.resident.coverage().event_body_limit,
        SCHEDULER_EVENT_BODY_WINDOW
    );
    let old = original.resident.capture(&request())?;
    assert_eq!(old.events.len(), 64);
    assert_eq!(old.recent_event_count, 100);
    assert_eq!(old.records.len(), 65); // One unresolved Queue and 64 shared Event bodies.
    let mut expanded = request();
    expanded.event_limit = 100;
    assert!(matches!(
        original
            .resident
            .capture(&expanded)
            .unwrap_err()
            .downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::WindowExpansion)
    ));
    let prepared = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &expanded, &budget)
    })?;
    assert_eq!(
        prepared.resident.coverage().revision,
        original.resident.coverage().revision
    );
    let fresh = prepared.resident.capture(&expanded)?;
    assert_eq!(fresh.events.len(), 100);
    assert_eq!(old.events.len(), 64);
    assert_eq!(
        fresh.events.last().unwrap().locator,
        old.events.last().unwrap().locator
    );
    drop((fresh, prepared, original, old));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn event_actual_noop_keeps_revision_and_raw_late_event_is_not_adopted() -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    let initial = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    let revision = initial.resident.coverage().revision;
    let original = match &initial.resident.capture(&request())?.events[0].fact {
        CurrentReadFact::Event(event) => event.clone(),
        _ => unreachable!(),
    };
    drop(initial);
    db.transaction(|tx| {
        let keys = BTreeSet::from([CurrentReadKey::event(&original.id)]);
        let mut proof =
            prepare_current_read_write_tx(tx, &scope(), &state, &keys, &request(), &budget)?;
        let (actual, ack) = append_current_read_event_tx(&mut proof, &original)?;
        assert_eq!(actual, original);
        let commit = register_current_read_writes_tx(proof, &[ack], &state)?;
        assert_eq!(commit.base_revision, revision);
        assert_eq!(commit.revision, revision);
        Ok(())
    })?;
    db.transaction(|tx| {
        let mut raw_late =
            AuditEvent::legacy("raw_late_event", serde_json::json!({"must_not_adopt":true}));
        raw_late.id = "raw-late-event".into();
        super::super::evidence::append_audit_event_tx(tx, Some(&scope().agent_id), &raw_late)?;
        let snapshot = prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)?;
        assert_eq!(snapshot.resident.capture(&request())?.recent_event_count, 1);
        let keys = BTreeSet::from([CurrentReadKey::event(&raw_late.id)]);
        let error = prepare_current_read_write_tx(tx, &scope(), &state, &keys, &request(), &budget)
            .err()
            .context("raw late must reject before business SQL")?;
        assert!(
            matches!(
                error.downcast_ref::<CurrentReadError>(),
                Some(CurrentReadError::NeedsPreparation)
            ),
            "{error:#}"
        );
        Ok(())
    })?;
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn missing_original_continuity_cannot_be_inferred_from_canonical_scope() -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM agent_episode_continuity WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
        )?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::NeedsPreparation)
        ),
        "{error:#}"
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_current", [], |r| r
                .get::<_, u64>(0))?,
        0
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn consumed_noop_proof_cannot_be_rebound_through_old_actual_ack_to_a_fresh_proof() -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    let initial = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    let original = match &initial.resident.capture(&request())?.events[0].fact {
        CurrentReadFact::Event(event) => event.clone(),
        _ => unreachable!(),
    };
    let revision = initial.resident.coverage().revision;
    drop(initial);
    db.transaction(|tx| {
        let keys = BTreeSet::from([CurrentReadKey::event(&original.id)]);
        let mut first =
            prepare_current_read_write_tx(tx, &scope(), &state, &keys, &request(), &budget)?;
        let (_, actual) = append_current_read_event_tx(&mut first, &original)?;
        let actual = [actual];
        let ack = register_current_read_writes_tx(first, &actual, &state)?;
        assert_eq!(ack.revision, revision);
        let second =
            prepare_current_read_write_tx(tx, &scope(), &state, &keys, &request(), &budget)?;
        let error = register_current_read_writes_tx(second, &actual, &state)
            .err()
            .context("old actual ACK cannot mint a new proof authority")?;
        assert!(
            format!("{error:#}").contains("actual ACK differs from original proof"),
            "{error:#}"
        );
        assert_eq!(
            tx.query_row("SELECT revision FROM current_read_current", [], |r| r
                .get::<_, u64>(0))?,
            revision
        );
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, u64>(0))?,
            0
        );
        Ok(())
    })?;
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn fixed_current_manifest_replace_and_update_target_cannot_reopen_qualified_cohort() -> Result<()> {
    for recursive in [false, true] {
        let (_home, db, state) = legacy()?;
        let budget = ledger()?;
        db.transaction(|tx| {
            tx.execute_batch(if recursive {"PRAGMA recursive_triggers=ON"} else {"PRAGMA recursive_triggers=OFF"})?;
            assert_eq!(tx.query_row("PRAGMA foreign_keys",[],|r|r.get::<_,u8>(0))?,1);
            let (seed,phase):(String,String)=tx.query_row("SELECT seed_id,capture_phase FROM current_read_fixed_captures WHERE sealed=1 LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let count:u64=tx.query_row("SELECT COUNT(*) FROM current_read_fixed_members",[],|r|r.get(0))?;
            assert!(tx.execute("INSERT OR REPLACE INTO current_read_fixed_captures(seed_id,capture_phase,queue_available,event_available,member_count,sealed) VALUES(?1,?2,1,1,0,0)",params![seed,phase]).is_err());
            tx.execute("INSERT INTO current_read_fixed_captures(seed_id,capture_phase,queue_available,event_available,member_count,sealed) VALUES(?1,'legacy_import',1,1,0,0)",[&seed])?;
            assert!(tx.execute("UPDATE OR REPLACE current_read_fixed_captures SET capture_phase=?2 WHERE seed_id=?1 AND capture_phase='legacy_import'",params![seed,phase]).is_err());
            assert!(tx.execute("INSERT INTO current_read_fixed_members(seed_id,capture_phase,family,record_id,agent_id,content_digest,projection_digest,status,sort_seq,sort_at,created_at) VALUES(?1,?2,'event','raw-late',?3,?4,?4,'raw',9,'0','0')",params![seed,phase,scope().agent_id,"a".repeat(64)]).is_err());
            assert_eq!(tx.query_row("SELECT sealed FROM current_read_fixed_captures WHERE seed_id=?1 AND capture_phase=?2",params![seed,phase],|r|r.get::<_,u8>(0))?,1);
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM current_read_fixed_members",[],|r|r.get::<_,u64>(0))?,count);
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM current_read_fixed_members WHERE record_id='raw-late'",[],|r|r.get::<_,u64>(0))?,0);
            Ok(())
        })?;
        let loaded = db.transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
        })?;
        assert_eq!(loaded.resident.capture(&request())?.events.len(), 1);
        drop(loaded);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
#[test]
fn normalized_current_read_columns_are_scalar_budgeted_before_projection_and_bad_payload(
) -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    drop(db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?);
    let before =
        db.connection()?
            .query_row("SELECT coverage_json FROM current_read_current", [], |r| {
                r.get::<_, String>(0)
            })?;
    for family in [CurrentReadFamily::Queue, CurrentReadFamily::Event] {
        let key = if family == CurrentReadFamily::Queue {
            CurrentReadKey::queue("original-current-read-input")
        } else {
            CurrentReadKey::event("original-current-read-event")
        };
        let error: Result<()> = db.transaction(|tx| {
            let (table, column, body, id) = if family == CurrentReadFamily::Queue {
                ("queue_entries", "priority", "payload_json", "message_id")
            } else {
                ("audit_events", "event_seq", "data_json", "audit_event_id")
            };
            tx.execute(
                &format!("UPDATE {table} SET {column}=?2,{body}='{{bad payload' WHERE {id}=?1"),
                params![key.id, "\\\"".repeat(65_536)],
            )?;
            let mut narrow = request();
            narrow.byte_limit = 4096;
            let scope = scope();
            let mut material = Material::new(&scope, &narrow, &budget);
            let error = stored::read(tx, &key, &scope.agent_id, &mut material)
                .err()
                .context("normalized raw gate must precede native projection/decode")?;
            assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::ResourceExhausted)
                ),
                "{error:#}"
            );
            drop(material);
            assert_eq!(budget.report()?.observed_bytes, 0);
            anyhow::bail!("rollback normalized corruption fixture")
        });
        assert!(
            format!("{:#}", error.unwrap_err()).contains("rollback normalized corruption fixture")
        );
        assert_eq!(
            db.connection()?.query_row(
                "SELECT coverage_json FROM current_read_current",
                [],
                |r| r.get::<_, String>(0)
            )?,
            before
        );
    }
    drop(db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn fixed_fingerprint_bounds_raw_normalized_affinity_columns_before_native_projection() -> Result<()>
{
    let (_home, db, _state) = legacy()?;
    let result:Result<()>=db.transaction(|tx| {
        tx.execute("UPDATE audit_events SET event_seq=?1,data_json='{bad payload' WHERE audit_event_id='original-current-read-event'",["x".repeat(CURRENT_READ_MAX_BYTES/8+1)])?;
        let error=stored::fixed_fingerprint(tx,&CurrentReadKey::event("original-current-read-event"),&scope().agent_id).err().context("fixed structural gate must precede projection/decode")?;
        assert!(matches!(error.downcast_ref::<CurrentReadError>(),Some(CurrentReadError::ResourceExhausted)),"{error:#}");
        anyhow::bail!("rollback fixed normalized corruption fixture")
    });
    assert!(format!("{:#}", result.unwrap_err())
        .contains("rollback fixed normalized corruption fixture"));
    Ok(())
}

#[test]
fn real_state_only_write_marks_current_read_unprepared_without_adopting_members() -> Result<()> {
    let (_home, db, before) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &before, &request(), &budget)
    })?;
    let aliases: usize =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| {
                r.get(0)
            })?;
    let old_revision = old.resident.coverage().revision;
    let mut after = before.clone();
    after.total_message_count = before.total_message_count + 1;
    db.transaction(|tx| super::super::evidence::upsert_agent_state_tx(tx, &after))?;
    let marker: (String, u64) = db.connection()?.query_row("SELECT freshness,revision FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=1", [&scope().agent_id], |r|Ok((r.get(0)?,r.get(1)?)))?;
    assert_eq!(marker, ("binding_invalidated".into(), old_revision));
    assert!(
        db.transaction(|tx| prepare_current_read_snapshot_tx(
            tx,
            &scope(),
            &before,
            &request(),
            &budget
        ))
        .is_err(),
        "old State cannot replace the actual canonical State"
    );
    let prepared = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &after, &request(), &budget)
    })?;
    assert_eq!(prepared.resident.coverage().revision, old_revision + 1);
    assert_eq!(
        prepared.resident.coverage().events,
        old.resident.coverage().events
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        aliases
    );
    assert_eq!(
        db.connection()?.query_row(
            "SELECT freshness FROM current_read_current WHERE agent_id=?1",
            [&scope().agent_id],
            |r| r.get::<_, String>(0)
        )?,
        "current"
    );
    drop((prepared, old));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_state_core_finalizer_clears_marker_with_one_revision_and_no_new_alias() -> Result<()> {
    let (_home, db, before) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &before, &request(), &budget)
    })?;
    let mut after = before.clone();
    after.total_message_count = before.total_message_count + 1;
    let committed = db.transaction(|tx| {
        let proof = prepare_current_read_write_tx(
            tx,
            &scope(),
            &before,
            &BTreeSet::new(),
            &request(),
            &budget,
        )?;
        super::super::evidence::upsert_agent_state_tx(tx, &after)?;
        assert_eq!(
            tx.query_row(
                "SELECT freshness FROM current_read_current WHERE agent_id=?1",
                [&scope().agent_id],
                |r| r.get::<_, String>(0)
            )?,
            "binding_invalidated"
        );
        register_current_read_writes_tx(proof, &[], &after)
    })?;
    assert_eq!(committed.base_revision, old.resident.coverage().revision);
    assert_eq!(committed.revision, committed.base_revision + 1);
    assert_eq!(
        committed.snapshot.resident.coverage().events,
        old.resident.coverage().events
    );
    let saved = db.connection()?.query_row(
        "SELECT coverage_json FROM current_read_current WHERE agent_id=?1",
        [&scope().agent_id],
        |r| r.get::<_, String>(0),
    )?;
    db.transaction(|tx| super::super::evidence::upsert_agent_state_tx(tx, &after))?;
    assert_eq!(
        db.connection()?.query_row(
            "SELECT freshness,coverage_json FROM current_read_current WHERE agent_id=?1",
            [&scope().agent_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        )?,
        ("current".into(), saved)
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    drop((committed, old));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn state_invalidation_rolls_back_and_retired_scope_never_reads_successor_state() -> Result<()> {
    let (_home, db, before) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &before, &request(), &budget)
    })?;
    let saved: (String, String) = db.connection()?.query_row(
        "SELECT freshness,coverage_json FROM current_read_current",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut after = before.clone();
    after.total_message_count = before.total_message_count + 1;
    let failed: Result<()> = db.transaction(|tx| {
        super::super::evidence::upsert_agent_state_tx(tx, &after)?;
        anyhow::bail!("actual State transaction rollback")
    });
    assert!(failed.is_err());
    assert_eq!(
        db.connection()?.query_row(
            "SELECT freshness,coverage_json FROM current_read_current",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        )?,
        saved
    );
    let poison = "{not-json".repeat(8192);
    db.transaction(|tx| {
        // Deliberate restored successor fixture, not a qualified current ACK.
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1", [&scope().agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2", rusqlite::params![poison,scope().agent_id])?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            prepare_original_current_read_recovery_tx(tx, &scope(), &before, &request(), &budget)
        })
        .err()
        .context("old scope cannot observe successor State")?;
    assert!(matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::RetiredScope)
    ));
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    drop(old);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn same_incarnation_deleting_ack_invalidates_binding_but_keeps_original_cleanup_strict(
) -> Result<()> {
    let (_home, db, state) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    let identity = db.agent_identities().latest(&scope().agent_id)?.unwrap();
    db.transaction(|tx| {
        repositories::ensure_agent_deletion_at_tx(
            tx,
            &repositories::AgentDeletionRequest {
                agent_id: scope().agent_id,
                admission: repositories::AgentDeletionAdmission::ExpectedRevision(
                    identity.revision,
                ),
                requested_by: "operator".into(),
                cascade_private_children: false,
            },
            "current-read-original-deletion",
            at() + chrono::Duration::seconds(1),
        )?;
        Ok(())
    })?;
    assert_eq!(
        db.agent_identities()
            .latest(&scope().agent_id)?
            .unwrap()
            .status,
        AgentRegistryStatus::Deleting
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT freshness FROM current_read_current", [], |r| r
                .get::<_, String>(
                0
            ))?,
        "binding_invalidated"
    );
    let denied = db
        .transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
        })
        .err()
        .context("ordinary Active read must remain denied")?;
    assert!(matches!(
        denied.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::RetiredScope)
    ));
    let cleanup = db.transaction(|tx| {
        prepare_original_current_read_recovery_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    assert_eq!(
        cleanup.resident.coverage().revision,
        old.resident.coverage().revision + 1
    );
    assert_eq!(
        cleanup.resident.coverage().events,
        old.resident.coverage().events
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    drop((old, cleanup));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn invalidated_binding_and_original_business_ack_share_one_revision() -> Result<()> {
    let (_home, db, before) = legacy()?;
    let budget = ledger()?;
    let old = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &before, &request(), &budget)
    })?;
    let mut after = before.clone();
    after.total_message_count += 1;
    db.transaction(|tx| super::super::evidence::upsert_agent_state_tx(tx, &after))?;
    // A dropped preSQL proof must not turn an unacknowledged rebind Current.
    db.transaction(|tx| {
        let proof = prepare_current_read_write_tx(
            tx,
            &scope(),
            &after,
            &BTreeSet::new(),
            &request(),
            &budget,
        )?;
        drop(proof);
        assert_eq!(
            tx.query_row(
                "SELECT revision,freshness FROM current_read_current",
                [],
                |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
            )?,
            (
                old.resident.coverage().revision,
                "binding_invalidated".into()
            )
        );
        Ok(())
    })?;
    let result: Result<()> = db.transaction(|tx| {
        let proof = prepare_current_read_write_tx(
            tx,
            &scope(),
            &after,
            &BTreeSet::new(),
            &request(),
            &budget,
        )?;
        let _facts = register_current_read_writes_tx(proof, &[], &after)?;
        anyhow::bail!("original whole receipt rolls back")
    });
    assert!(result.is_err());
    assert_eq!(
        db.connection()?.query_row(
            "SELECT revision,freshness FROM current_read_current",
            [],
            |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
        )?,
        (
            old.resident.coverage().revision,
            "binding_invalidated".into()
        )
    );
    let facts = db.transaction(|tx| {
        let proof = prepare_current_read_write_tx(
            tx,
            &scope(),
            &after,
            &BTreeSet::new(),
            &request(),
            &budget,
        )?;
        register_current_read_writes_tx(proof, &[], &after)
    })?;
    assert_eq!(facts.base_revision, old.resident.coverage().revision);
    assert_eq!(facts.revision, facts.base_revision + 1);
    assert_eq!(
        facts.snapshot.resident.coverage().events,
        old.resident.coverage().events
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    assert_eq!(
        db.connection()?
            .query_row("SELECT freshness FROM current_read_current", [], |r| r
                .get::<_, String>(
                0
            ))?,
        "current"
    );
    drop((facts, old));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[path = "state_source_tests.rs"]
mod state_source_tests;

#[test]
fn original_fixed_unavailable_does_not_hide_later_actual_accepted_event_corruption() -> Result<()> {
    let (_home, db, state) = legacy_events_with_event_source(0, false)?;
    let budget = ledger()?;
    let absent: u64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM current_read_fixed_captures WHERE event_available=0 AND sealed=1",
        [],
        |r| r.get(0),
    )?;
    assert!(
        absent > 0,
        "original missing table must stay fixed unavailable"
    );
    // The actual original table returns later; immutable cutover still records
    // its absence. New business sources have independent exact native authority.
    db.connection()?.execute_batch("CREATE TABLE audit_events(audit_event_id TEXT PRIMARY KEY,event_seq INTEGER,agent_id TEXT,kind TEXT NOT NULL,created_at TEXT NOT NULL,data_json TEXT NOT NULL)")?;
    let mut event = AuditEvent::legacy(
        "after_fixed_unavailable",
        serde_json::json!({"actual":true}),
    );
    event.id = "actual-event-after-fixed-unavailable".into();
    event.created_at = at();
    let (facts, actual_event) = db.transaction(|tx| {
        let keys = BTreeSet::from([CurrentReadKey::event(&event.id)]);
        let mut business = CurrentReadBusinessTransaction::prepare_original_maintenance_tx(
            tx,
            &scope(),
            &state,
            &keys,
            &request(),
            &budget,
            (0, 0),
            vec![],
        )?;
        let (actual, inserted) = business.append_event_tx(tx, Some(&scope().agent_id), &event)?;
        assert!(inserted);
        Ok((business.finish_original_maintenance(&state)?, actual))
    })?;
    assert!(facts.resident().is_none());
    let revision = facts.revision;
    drop(facts);
    assert_eq!(budget.report()?.observed_bytes, 0);
    for oversized in [false, true] {
        db.connection()?.execute(
            "UPDATE audit_events SET data_json=?1 WHERE audit_event_id=?2",
            params![
                if oversized {
                    "{".repeat(9 * 1024 * 1024)
                } else {
                    serde_json::to_string(&AuditEvent {
                        data: serde_json::json!({"actual":false}),
                        ..actual_event.clone()
                    })?
                },
                event.id
            ],
        )?;
        let error = db
            .transaction(|tx| {
                prepare_original_current_read_recovery_tx(tx, &scope(), &state, &request(), &budget)
            })
            .unwrap_err();
        if oversized {
            assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::ResourceExhausted)
                ),
                "{error:#}"
            );
        } else {
            assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::CanonicalChanged)
                ),
                "{error:#}"
            );
        }
        let actual: u64 =
            db.connection()?
                .query_row("SELECT revision FROM current_read_current", [], |r| {
                    r.get(0)
                })?;
        assert_eq!(actual, revision);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[path = "event_birth_tests.rs"]
mod event_birth_tests;
#[path = "event_query_tests.rs"]
mod event_query_tests;
