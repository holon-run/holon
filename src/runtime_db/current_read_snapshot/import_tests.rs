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
fn at() -> chrono::DateTime<chrono::Utc> {
    "2026-10-01T00:00:00.123456789Z".parse().unwrap()
}
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "current-import-agent".into(),
        identity_incarnation: 1,
    }
}
fn request() -> CurrentReadRequest {
    CurrentReadRequest::current(CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS)
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn queue(id: &str) -> QueueEntryRecord {
    QueueEntryRecord {
        message_id: id.into(),
        agent_id: scope().agent_id,
        priority: Priority::Next,
        status: QueueEntryStatus::Queued,
        created_at: at(),
        updated_at: at(),
    }
}
fn event(id: &str) -> AuditEvent {
    let mut e = AuditEvent::legacy("current_import", serde_json::json!({"original":true}));
    e.id = id.into();
    e.created_at = at();
    e
}
fn source(id: &str) -> MessageEnvelope {
    let mut m = MessageEnvelope::new(
        scope().agent_id,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "original fixed source".into(),
        },
    );
    m.id = id.into();
    m.created_at = at();
    m
}
// Apply the real v81 boundary and the real original importer, never synthetic aliases.
fn legacy(import_before_domain: bool) -> Result<(tempfile::TempDir, RuntimeDb, AgentState)> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("runtime.sqlite");
    let mut connection = Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for m in MIGRATIONS.iter().filter(|m| m.version <= 81) {
        apply_migration(&mut connection, m)?;
    }
    super::super::ensure_runtime_identity_metadata(&connection)?;
    let state = AgentState::new(&scope().agent_id);
    {
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
        upsert_agent_state_tx(&tx, &state)?;
        for id in [
            "fixed-source",
            "import-source",
            "older-source",
            "missing-schema-source",
        ] {
            append_message_tx(&tx, &source(id))?;
        }
        repositories::upsert_queue_entry_tx(&tx, &queue("fixed-source"))?;
        tx.commit()?;
    }
    for m in MIGRATIONS
        .iter()
        .filter(|m| m.version > 81 && m.version < 90)
    {
        apply_migration(&mut connection, m)?;
    }
    if import_before_domain {
        let tx = connection.transaction()?;
        super::super::episode_snapshot::seal_imported_episode_continuity_tx(&tx, &[], at())?;
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM episode_continuity_imports", [], |r| r
                .get::<_, usize>(0))?,
            1
        );
        tx.commit()?;
    }
    for m in MIGRATIONS.iter().filter(|m| m.version >= 90) {
        apply_migration(&mut connection, m)?;
    }
    drop(connection);
    let db = RuntimeDb::open_and_migrate(path, home.path().join("runtime.lock"))?;
    Ok((home, db, state))
}
#[test]
fn original_repository_import_seals_only_received_queue_and_actual_event_readback() -> Result<()> {
    let (_home, db, state) = legacy(false)?;
    let budget = ledger()?;
    let pending = db
        .transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
        })
        .err()
        .context("original import is still pending")?;
    assert!(matches!(
        pending.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::NeedsPreparation)
    ));
    assert_eq!(budget.report()?.observed_bytes, 0);
    db.queue_entries()
        .import_legacy(vec![queue("import-source")])?;
    let raw_late = event("raw-late-event");
    db.audit_events()
        .append(Some(&scope().agent_id), &raw_late)?;
    db.audit_events()
        .import_legacy(Some(&scope().agent_id), vec![event("received-event")])?;
    db.context_episodes().import_legacy(Vec::new())?;
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM current_read_fixed_captures WHERE capture_phase='first_import' AND sealed=1",[],|r|r.get::<_,usize>(0))?,1);
    let snapshot = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    let selected = snapshot.resident.capture(&request())?;
    assert_eq!(selected.queue_counts["queued"], 2);
    assert_eq!(selected.events.len(), 1);
    assert_eq!(selected.events[0].locator.key.id, "received-event");
    let actual = db
        .audit_events()
        .recent(Some(&scope().agent_id), 4)?
        .into_iter()
        .find(|e| e.id == "received-event")
        .unwrap();
    assert!(actual.event_seq > 0 && !actual.event_log_epoch.is_empty());
    assert_eq!(db.connection()?.query_row("SELECT content_digest FROM current_read_import_inventory WHERE family='event' AND record_id='received-event'",[],|r|r.get::<_,String>(0))?,typed_digest(&actual)?);
    let frozen: usize = db.connection()?.query_row(
        "SELECT COUNT(*) FROM current_read_fixed_members WHERE capture_phase='first_import'",
        [],
        |r| r.get(0),
    )?;
    db.audit_events()
        .append(Some(&scope().agent_id), &event("after-seal-event"))?;
    db.context_episodes().import_legacy(Vec::new())?;
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM current_read_fixed_members WHERE capture_phase='first_import'",
            [],
            |r| r.get::<_, usize>(0)
        )?,
        frozen
    );
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM current_read_fixed_members WHERE record_id IN('raw-late-event','after-seal-event')",[],|r|r.get::<_,usize>(0))?,0);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    drop((snapshot, selected));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn original_import_completed_before_domain_install_is_sealed_at_actual_upgrade() -> Result<()> {
    let (_home, db, state) = legacy(true)?;
    let budget = ledger()?;
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM current_read_fixed_captures WHERE capture_phase='first_import' AND sealed=1",[],|r|r.get::<_,usize>(0))?,1);
    let snapshot = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    assert_eq!(snapshot.resident.coverage().pending_queue.len(), 1);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    drop(snapshot);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn rejected_older_queue_import_cannot_qualify_the_newer_raw_canonical_row() -> Result<()> {
    let (_home, db, state) = legacy(false)?;
    let budget = ledger()?;
    let mut newer = queue("older-source");
    newer.updated_at += chrono::Duration::seconds(2);
    db.transaction(|tx| repositories::upsert_queue_entry_tx(tx, &newer).map(|_| ()))?;
    db.queue_entries()
        .import_legacy(vec![queue("older-source")])?;
    assert_eq!(db.queue_entries().latest("older-source")?, Some(newer));
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM current_read_import_inventory WHERE record_id='older-source'",
            [],
            |r| r.get::<_, usize>(0)
        )?,
        0
    );
    db.context_episodes().import_legacy(Vec::new())?;
    assert_eq!(
        db.connection()?.query_row(
            "SELECT COUNT(*) FROM current_read_fixed_members WHERE record_id='older-source'",
            [],
            |r| r.get::<_, usize>(0)
        )?,
        0
    );
    let error = db
        .transaction(|tx| {
            prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
        })
        .err()
        .context("unreceived raw Queue cannot be complete")?;
    assert!(matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::UnknownNamespace(CurrentReadFamily::Queue))
    ));
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn installed_missing_current_import_schema_rolls_back_original_repository_import() -> Result<()> {
    let (_home, db, _state) = legacy(false)?;
    db.transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE current_read_fixed_captures RENAME TO missing_current_fixed_captures",
        )?;
        Ok(())
    })?;
    let before: usize =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM queue_entries", [], |r| r.get(0))?;
    let error = db
        .queue_entries()
        .import_legacy(vec![queue("missing-schema-source")])
        .err()
        .context("installed schema loss cannot silently skip qualification")?;
    assert!(format!("{error:#}").contains("CurrentRead installed import schema missing"));
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM queue_entries", [], |r| r
                .get::<_, usize>(0))?,
        before
    );
    assert!(db
        .queue_entries()
        .latest("missing-schema-source")?
        .is_none());
    assert!(!db.storage_domain_is_complete("queue_entries", "db")?);
    Ok(())
}

#[test]
fn original_post_upgrade_legacy_identity_seed_uses_only_actual_received_import_inventory(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| append_message_tx(tx, &source("import-source")).map(|_| ()))?;
    let identity = AgentIdentityRecord::new(
        scope().agent_id,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    db.agent_identities().import_legacy(vec![identity])?;
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM current_read_fixed_captures WHERE capture_phase='legacy_import' AND sealed=1",[],|r|r.get::<_,usize>(0))?,1);
    db.queue_entries()
        .import_legacy(vec![queue("import-source")])?;
    let state = AgentState::new(&scope().agent_id);
    db.agent_states().import_legacy(Some(state.clone()))?;
    db.audit_events().import_legacy(
        Some(&scope().agent_id),
        vec![event("legacy-received-event")],
    )?;
    db.context_episodes().import_legacy(Vec::new())?;
    let budget = ledger()?;
    let snapshot = db.transaction(|tx| {
        prepare_current_read_snapshot_tx(tx, &scope(), &state, &request(), &budget)
    })?;
    assert_eq!(snapshot.resident.coverage().pending_queue.len(), 1);
    assert_eq!(snapshot.resident.coverage().events.len(), 1);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_members", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    drop(snapshot);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}
