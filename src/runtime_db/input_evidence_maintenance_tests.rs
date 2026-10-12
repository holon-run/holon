use super::*;
use crate::runtime_db::evidence::{
    append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx, upsert_message_tx,
};
use crate::types::{
    AgentKind, AgentOwnership, AgentProfilePreset, AgentState, AgentVisibility, AuthorityClass,
    MessageBody, MessageKind, MessageOrigin, Priority,
};
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    }
}
fn fixture(qualify: bool) -> Result<(tempfile::TempDir, RuntimeDb, MessageEnvelope)> {
    let directory = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )?;
    let message = db.transaction(|tx| {
        let identity = AgentIdentityRecord::new(
            "agent-a",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        upsert_agent_identity_tx(tx, &identity)?;
        upsert_agent_state_tx(tx, &AgentState::new("agent-a"))?;
        let message = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "before".into(),
            },
        );
        let (message, _) = append_message_tx(tx, &message)?;
        if qualify {
            register_committed_input_evidence_tx(
                tx,
                &scope(),
                &[CanonicalInputEvidence::Message(message.clone())],
            )?;
        }
        Ok(message)
    })?;
    Ok((directory, db, message))
}
fn changed(original: &MessageEnvelope) -> MessageEnvelope {
    let mut message = original.clone();
    message.body = MessageBody::Text {
        text: "after".into(),
    };
    message
}
fn message_key(message: &MessageEnvelope) -> Key {
    Key::new(Family::Message, &message.id)
}
fn stored_message(db: &RuntimeDb, id: &str) -> Result<MessageEnvelope> {
    db.transaction(|tx| {
        let payload = payload_on(tx, "agent-a", &Key::new(Family::Message, id))?
            .context("fixture Message missing")?;
        Ok(serde_json::from_str(&payload)?)
    })
}

#[test]
fn qualified_repair_requires_capability_and_rolls_back_original_writes() -> Result<()> {
    let (_directory, db, original) = fixture(true)?;
    let result = db.transaction(|tx| {
        let prepared = prepare_offline_input_repair_tx(
            tx,
            &db,
            "agent-a",
            &[message_key(&original)],
            &[],
            None,
        )?;
        upsert_message_tx(tx, &changed(&original))?;
        prepared.finish_tx(tx)
    });
    assert!(result
        .unwrap_err()
        .downcast_ref::<NeedsOfflineInputMaintenance>()
        .is_some());
    assert_eq!(stored_message(&db, &original.id)?, original);
    Ok(())
}
#[test]
fn maintenance_updates_current_alias_from_original_transaction_exact_readback() -> Result<()> {
    let (directory, db, original) = fixture(true)?;
    let lock_path = directory.path().join("runtime-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&lock_path)?;
    let authority = OfflineInputMaintenance::for_database(&db, &lease, &lock_path)?;
    let delta = db.transaction(|tx| {
        let prepared = prepare_offline_input_repair_tx(
            tx,
            &db,
            "agent-a",
            &[message_key(&original)],
            &[],
            Some(&authority),
        )?;
        upsert_message_tx(tx, &changed(&original))?;
        prepared.finish_tx(tx)
    })?;
    assert_eq!(delta.records.len(), 1);
    db.transaction(|tx| {
        validate_existing_input_evidence_tx(tx, &scope(), &message_key(&original))
    })?;
    assert_eq!(stored_message(&db, &original.id)?, changed(&original));
    Ok(())
}
#[test]
fn maintenance_never_qualifies_fresh_same_name_historical_row() -> Result<()> {
    let (directory, db, original) = fixture(false)?;
    let lock_path = directory.path().join("runtime-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&lock_path)?;
    let authority = OfflineInputMaintenance::for_database(&db, &lease, &lock_path)?;
    let delta = db.transaction(|tx| {
        let prepared = prepare_offline_input_repair_tx(
            tx,
            &db,
            "agent-a",
            &[message_key(&original)],
            &[],
            Some(&authority),
        )?;
        upsert_message_tx(tx, &changed(&original))?;
        prepared.finish_tx(tx)
    })?;
    assert!(delta.records.is_empty());
    assert!(!db.transaction(|tx| existing_input_evidence_member_tx(
        tx,
        &scope(),
        &message_key(&original)
    ))?);
    Ok(())
}
#[test]
fn failure_after_qualification_rolls_back_canonical_body_and_alias_revision() -> Result<()> {
    let (directory, db, original) = fixture(true)?;
    let lock_path = directory.path().join("runtime-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&lock_path)?;
    let authority = OfflineInputMaintenance::for_database(&db, &lease, &lock_path)?;
    let before = db
        .transaction(|tx| alias_on(tx, &scope(), &message_key(&original)))?
        .unwrap();
    let result: Result<()> = db.transaction(|tx| {
        let prepared = prepare_offline_input_repair_tx(
            tx,
            &db,
            "agent-a",
            &[message_key(&original)],
            &[],
            Some(&authority),
        )?;
        upsert_message_tx(tx, &changed(&original))?;
        prepared.finish_tx(tx)?;
        anyhow::bail!("rollback after qualification")
    });
    assert!(result.is_err());
    let after = db
        .transaction(|tx| alias_on(tx, &scope(), &message_key(&original)))?
        .unwrap();
    assert_eq!(before.revision, after.revision);
    assert_eq!(before.fingerprint, after.fingerprint);
    assert_eq!(stored_message(&db, &original.id)?, original);
    Ok(())
}
#[test]
fn maintenance_capability_rejects_wrong_lock_path_and_database() -> Result<()> {
    let (directory, db, original) = fixture(true)?;
    let lock_path = directory.path().join("runtime-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&lock_path)?;
    assert!(OfflineInputMaintenance::for_database(
        &db,
        &lease,
        &directory.path().join("other.lock")
    )
    .is_err());
    let (_other_directory, other, _) = fixture(false)?;
    let authority = OfflineInputMaintenance::for_database(&db, &lease, &lock_path)?;
    assert!(other
        .transaction(|tx| prepare_offline_input_repair_tx(
            tx,
            &other,
            "agent-a",
            &[message_key(&original)],
            &[],
            Some(&authority)
        ))
        .is_err());
    Ok(())
}

#[test]
fn maintenance_advances_only_fixed_unbound_seed_without_inventing_identity() -> Result<()> {
    use crate::runtime_db::migrations::{apply_migration, ensure_migration_table, MIGRATIONS};
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime.sqlite");
    let mut connection = rusqlite::Connection::open(&path)?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS.iter().filter(|m| m.version <= 81) {
        apply_migration(&mut connection, migration)?;
    }
    let original = {
        let tx = connection.transaction()?;
        upsert_agent_state_tx(&tx, &AgentState::new("agent-a"))?;
        tx.execute("INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at) VALUES('context_episode_anchors',1,'complete','db','2026-10-12T00:00:00Z')", [])?;
        let message = MessageEnvelope::new(
            "agent-a",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "before".into(),
            },
        );
        let (message, _) = append_message_tx(&tx, &message)?;
        tx.commit()?;
        message
    };
    drop(connection);
    let db = RuntimeDb::open_and_migrate(&path, directory.path().join("runtime.lock"))?;
    let seed =
        db.transaction(|tx| legacy_recovery::legacy_seed_recovery_scope_tx(tx, "agent-a"))?;
    let original_fingerprint: String = db.connection()?.query_row("SELECT fingerprint_json FROM input_evidence_eligibility WHERE seed_id=?1 AND family='message' AND record_id=?2", params![seed.seed_id, original.id], |r| r.get(0))?;
    let lock_path = directory.path().join("runtime-maintenance.lock");
    let lease = RuntimeDbLock::try_lock(&lock_path)?;
    let authority = OfflineInputMaintenance::for_database(&db, &lease, &lock_path)?;
    db.transaction(|tx| {
        let prepared = prepare_offline_input_repair_tx(
            tx,
            &db,
            "agent-a",
            &[message_key(&original)],
            &[],
            Some(&authority),
        )?;
        upsert_message_tx(tx, &changed(&original))?;
        prepared.finish_tx(tx)?;
        let current = fingerprint_on(tx, "agent-a", &message_key(&original))?.unwrap();
        require_eligible(
            tx,
            &Binding {
                seed_id: Some(seed.seed_id.clone()),
            },
            &message_key(&original),
            &current,
        )?;
        Ok(())
    })?;
    let unchanged: String = db.connection()?.query_row("SELECT fingerprint_json FROM input_evidence_eligibility WHERE seed_id=?1 AND family='message' AND record_id=?2", params![seed.seed_id, original.id], |r| r.get(0))?;
    assert_eq!(original_fingerprint, unchanged);
    let identity_count: i64 = db.connection()?.query_row(
        "SELECT COUNT(*) FROM agent_identities WHERE agent_id='agent-a'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(identity_count, 0);
    Ok(())
}
