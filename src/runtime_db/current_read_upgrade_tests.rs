use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        current_query::{prepare, prepare_event_page, CurrentSelection},
        current_read_writes,
        evidence::{
            append_audit_event_tx, append_message_tx, upsert_agent_identity_tx,
            upsert_agent_state_tx,
        },
        migrations::{apply_migration, ensure_migration_table, Migration, MIGRATIONS},
        repositories::upsert_queue_entry_tx,
    },
    types::{
        AgentKind, AgentOwnership, AgentProfilePreset, AgentState, AgentVisibility, AuditEvent,
        AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority,
    },
};

const AGENT: &str = "current-upgrade";

fn migrate_to(connection: &mut Connection, version: i64) -> Result<()> {
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= version)
    {
        apply_migration(connection, migration)?;
    }
    Ok(())
}

fn current_migration() -> &'static Migration {
    MIGRATIONS
        .iter()
        .find(|migration| migration.version == current_read_writes::VERSION)
        .unwrap()
}

fn episode_migration_version() -> i64 {
    MIGRATIONS
        .iter()
        .find(|migration| migration.name == "explicit_episode_continuity")
        .unwrap()
        .version
}

fn freeze_episode_cut(connection: &mut Connection) -> Result<()> {
    migrate_to(connection, episode_migration_version())?;
    let roots: String = connection.query_row(
        "SELECT roots_json FROM episode_continuity_seeds WHERE agent_id=?1",
        [AGENT],
        |row| row.get(0),
    )?;
    let roots: EpisodeContinuityRoots = serde_json::from_str(&roots)?;
    assert!(roots.record_digests.contains_key("queue:source-00"));
    assert!(roots.record_digests.contains_key("message:source-00"));
    Ok(())
}

fn legacy(count: usize) -> Result<(Connection, Vec<MessageEnvelope>)> {
    let mut connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    migrate_to(&mut connection, 81)?;
    let tx = connection.transaction()?;
    upsert_agent_identity_tx(
        &tx,
        &AgentIdentityRecord::new(
            AGENT,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ),
    )?;
    let mut state = AgentState::new(AGENT);
    state.pending = count;
    state.total_message_count = count;
    upsert_agent_state_tx(&tx, &state)?;
    tx.execute(
        "INSERT INTO runtime_metadata(key,value,created_at,updated_at)
         VALUES('event_log_epoch',?1,?2,?2)",
        params![crate::ids::event_log_epoch_id(), "2026-10-01T00:00:00Z"],
    )?;
    for domain in ["context_episode_anchors", "turn_records", "evidence"] {
        tx.execute(
            "INSERT INTO storage_domains(domain,schema_version,import_status,canonical_source,updated_at)
             VALUES(?1,1,'complete','db','2026-10-01T00:00:00Z')",
            [domain],
        )?;
    }
    let mut messages = Vec::new();
    for index in 0..count {
        let mut message = MessageEnvelope::new(
            AGENT,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original".into(),
            },
        );
        message.id = format!("source-{index:02}");
        message.normalize_admission_fields();
        let message = append_message_tx(&tx, &message)?.0;
        upsert_queue_entry_tx(
            &tx,
            &QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: AGENT.into(),
                priority: message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: message.created_at,
                updated_at: message.created_at,
            },
        )?;
        messages.push(message);
    }
    append_audit_event_tx(
        &tx,
        Some(AGENT),
        &AuditEvent::legacy("old-event", serde_json::json!({})),
    )?;
    tx.commit()?;
    Ok((connection, messages))
}

fn qualified_count(connection: &Connection) -> Result<usize> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='current_read_qualification')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM current_read_qualification",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn current_upgrade_adopts_only_fixed_pending_queues_and_no_event_history() -> Result<()> {
    let (mut connection, messages) = legacy(1)?;
    migrate_to(&mut connection, current_read_writes::VERSION)?;
    assert_eq!(qualified_count(&connection)?, 1);
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let selection = CurrentSelection {
        queue_ids: [messages[0].id.clone()].into_iter().collect(),
        ..Default::default()
    };
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    {
        let tx = connection.transaction()?;
        let query = prepare(&tx, &scope, Some(&selection), &ledger, (1024 * 1024, 1024))?;
        let _captured = query.capture(&selection, &ledger)?;
        assert!(query.queue_record(&messages[0].id)?.is_some());
        let history = CurrentSelection {
            events_after: Some(0),
            ..Default::default()
        };
        assert!(prepare(&tx, &scope, Some(&history), &ledger, (1024 * 1024, 1024)).is_err());
    }
    apply_migration(&mut connection, current_migration())?;
    assert_eq!(qualified_count(&connection)?, 1);
    Ok(())
}

#[test]
fn offline_event_before_history_schema_is_adopted_by_original_upgrade() -> Result<()> {
    let (mut connection, _messages) = legacy(1)?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let event = AuditEvent::legacy("offline-before-history", serde_json::json!({}));
    {
        let tx = connection.transaction()?;
        append_audit_event_tx(&tx, Some(AGENT), &event)?;
        current_read_writes::append_event_history_ack_tx(&tx, &scope, &event.id)?;
        tx.commit()?;
    }
    migrate_to(&mut connection, current_read_writes::VERSION)?;
    let acked: usize = connection.query_row(
        "SELECT COUNT(*) FROM event_history_acks
         WHERE agent_id=?1 AND identity_incarnation=?2 AND audit_event_id=?3",
        params![scope.agent_id, scope.identity_incarnation, event.id],
        |row| row.get(0),
    )?;
    assert_eq!(acked, 1);
    Ok(())
}

#[test]
fn current_upgrade_freezes_event_history_acks_for_the_finite_history_page() -> Result<()> {
    let (mut connection, _messages) = legacy(1)?;
    // Give the legacy database real Events before the Current cut.
    {
        let tx = connection.transaction()?;
        for index in 0..3 {
            let mut event =
                AuditEvent::legacy("legacy_history", serde_json::json!({"index": index}));
            event.id = format!("legacy-event-{index}");
            append_audit_event_tx(&tx, Some(AGENT), &event)?;
        }
        tx.commit()?;
    }
    let expected_ids = {
        let mut statement = connection.prepare(
            "SELECT audit_event_id FROM audit_events WHERE agent_id=?1 ORDER BY event_seq",
        )?;
        let ids = statement
            .query_map([AGENT], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    migrate_to(&mut connection, current_read_writes::VERSION)?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let acked: usize = connection.query_row(
        "SELECT COUNT(*) FROM event_history_acks WHERE audit_event_id LIKE 'legacy-event-%'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(acked, 3);
    {
        let tx = connection.transaction()?;
        // Live Current selection still refuses the legacy Event window.
        let history = CurrentSelection {
            events_after: Some(0),
            ..Default::default()
        };
        assert!(prepare(&tx, &scope, Some(&history), &ledger, (1024 * 1024, 1024)).is_err());
        // The finite history page reads the archived bodies through the ACKs.
        let page = prepare_event_page(
            &tx,
            &scope,
            &ledger,
            None,
            Some(0),
            10,
            crate::storage::EventLogPageOrder::Asc,
            |_| true,
        )?;
        let ids = page
            .events()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, expected_ids);
        drop(page);
        // A tampered archived body is rejected by its frozen digest.
        tx.execute(
            "UPDATE audit_events SET data_json=json_set(data_json,'$.data.index',999)
             WHERE audit_event_id='legacy-event-1'",
            [],
        )?;
        let refused = prepare_event_page(
            &tx,
            &scope,
            &ledger,
            None,
            Some(0),
            10,
            crate::storage::EventLogPageOrder::Asc,
            |_| true,
        )
        .err()
        .context("tampered archive must fail")?;
        assert!(format!("{refused:#}").contains("needs original writer preparation"));
    }
    Ok(())
}

#[test]
fn current_upgrade_does_not_adopt_a_later_raw_queue_address() -> Result<()> {
    let (mut connection, _) = legacy(1)?;
    freeze_episode_cut(&mut connection)?;
    connection.execute(
        "INSERT INTO queue_entries(message_id,agent_id,priority,status,created_at,updated_at,payload_json)
         SELECT 'late-queue',agent_id,priority,status,created_at,updated_at,
                json_set(payload_json,'$.message_id','late-queue') FROM queue_entries",
        [],
    )?;
    migrate_to(&mut connection, current_read_writes::VERSION)?;
    assert_eq!(qualified_count(&connection)?, 1);
    assert_eq!(
        connection.query_row(
            "SELECT COUNT(*) FROM current_read_qualification WHERE record_id='late-queue'",
            [],
            |row| row.get::<_, usize>(0),
        )?,
        0
    );
    Ok(())
}

#[test]
fn current_upgrade_rejects_fixed_queue_or_canonical_column_drift() -> Result<()> {
    for sql in [
        "UPDATE queue_entries SET payload_json=json_set(payload_json,'$.priority','background')",
        "UPDATE queue_entries SET status='processed'",
        "UPDATE queue_entries SET priority='background'",
        "UPDATE queue_entries SET updated_at='2026-12-01T00:00:00.000Z'",
        "UPDATE queue_entries SET agent_id='foreign'",
    ] {
        let (mut connection, _) = legacy(1)?;
        migrate_to(&mut connection, current_read_writes::VERSION - 1)?;
        connection.execute(sql, [])?;
        assert!(
            apply_migration(&mut connection, current_migration()).is_err(),
            "{sql}"
        );
        assert_eq!(qualified_count(&connection)?, 0);
    }
    Ok(())
}

#[test]
fn current_upgrade_rejects_message_rewrites_even_with_a_new_canonical_hash() -> Result<()> {
    for cut in [
        episode_migration_version(),
        current_read_writes::VERSION - 1,
    ] {
        let (mut connection, _) = legacy(1)?;
        freeze_episode_cut(&mut connection)?;
        migrate_to(&mut connection, cut)?;
        let original: String =
            connection.query_row("SELECT payload_json FROM messages", [], |row| row.get(0))?;
        let changed = original.replace("original", "tampered");
        assert_eq!(original.len(), changed.len());
        assert_ne!(original, changed);
        connection.execute(
            "UPDATE messages SET payload_json=?1,content_hash=?2",
            params![
                changed,
                format!("sha256:{}", bytes_digest(changed.as_bytes()))
            ],
        )?;
        assert!(
            migrate_to(&mut connection, current_read_writes::VERSION).is_err(),
            "cut={cut}"
        );
        assert_eq!(qualified_count(&connection)?, 0);
    }
    Ok(())
}

#[test]
fn current_upgrade_requires_original_mandatory_input_membership() -> Result<()> {
    for sql in [
        "UPDATE input_evidence_eligibility SET mandatory=0 WHERE family='message'",
        "DELETE FROM input_evidence_eligibility WHERE family='message'",
        "DELETE FROM input_evidence_captures",
        "DELETE FROM messages",
    ] {
        let (mut connection, _) = legacy(1)?;
        migrate_to(&mut connection, current_read_writes::VERSION - 1)?;
        connection.execute(sql, [])?;
        assert!(
            apply_migration(&mut connection, current_migration()).is_err(),
            "{sql}"
        );
        assert_eq!(qualified_count(&connection)?, 0);
    }
    Ok(())
}

#[test]
fn current_upgrade_never_borrows_another_or_unbound_identity_scope() -> Result<()> {
    for sql in [
        "UPDATE episode_continuity_seeds SET identity_incarnation=2",
        "UPDATE episode_continuity_seeds SET identity_incarnation=NULL",
        "UPDATE episode_continuity_seeds SET agent_id='foreign'",
        "UPDATE episode_continuity_seeds SET boundary_id='foreign'",
        "UPDATE episode_continuity_seeds SET source_kind='upgrade_unbound'",
        "UPDATE agent_episode_continuity SET mode='fresh_empty',seed_id=NULL",
        "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2)",
        "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.agent_id','foreign')",
        "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.status','deleted')",
    ] {
        let (mut connection, _) = legacy(1)?;
        migrate_to(&mut connection, current_read_writes::VERSION - 1)?;
        connection.execute(sql, [])?;
        let _ = apply_migration(&mut connection, current_migration());
        assert_eq!(qualified_count(&connection)?, 0, "{sql}");
    }
    Ok(())
}

#[test]
fn current_upgrade_rolls_back_earlier_qualifications_and_retries_the_original_cut() -> Result<()> {
    let (mut connection, messages) = legacy(2)?;
    migrate_to(&mut connection, current_read_writes::VERSION - 1)?;
    let original: String = connection.query_row(
        "SELECT payload_json FROM queue_entries WHERE message_id=?1",
        [&messages[1].id],
        |row| row.get(0),
    )?;
    connection.execute(
        "UPDATE queue_entries SET payload_json=json_set(payload_json,'$.priority','background') WHERE message_id=?1",
        [&messages[1].id],
    )?;
    assert!(apply_migration(&mut connection, current_migration()).is_err());
    assert_eq!(
        connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| row
            .get::<_, i64>(
            0
        ),)?,
        current_read_writes::VERSION - 1
    );
    assert_eq!(qualified_count(&connection)?, 0);
    connection.execute(
        "UPDATE queue_entries SET payload_json=?2 WHERE message_id=?1",
        params![messages[1].id, original],
    )?;
    apply_migration(&mut connection, current_migration())?;
    assert_eq!(qualified_count(&connection)?, 2);
    Ok(())
}

#[test]
fn current_upgrade_is_not_a_startup_requalification_path() -> Result<()> {
    let (mut connection, messages) = legacy(1)?;
    migrate_to(&mut connection, current_read_writes::VERSION)?;
    connection.execute(
        "UPDATE current_read_qualification SET qualification_state='unprepared',content_digest=NULL,revision=2",
        [],
    )?;
    apply_migration(&mut connection, current_migration())?;
    assert_eq!(
        connection.query_row(
            "SELECT qualification_state FROM current_read_qualification WHERE record_id=?1",
            [&messages[0].id],
            |row| row.get::<_, String>(0),
        )?,
        "unprepared"
    );
    let tx = connection.transaction()?;
    assert!(capture_tx(&tx).is_err());
    Ok(())
}

#[test]
fn current_upgrade_streams_event_histories_larger_than_one_record_page() -> Result<()> {
    let (mut connection, _) = legacy(1)?;
    migrate_to(&mut connection, current_read_writes::VERSION - 1)?;
    let original: usize = connection.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE agent_id=?1",
        [AGENT],
        |r| r.get(0),
    )?;
    {
        let tx = connection.transaction()?;
        let last: i64 = tx.query_row(
            "SELECT COALESCE(MAX(event_seq),0) FROM audit_events WHERE agent_id=?1",
            [AGENT],
            |r| r.get(0),
        )?;
        let mut insert = tx.prepare(
            "INSERT INTO audit_events(audit_event_id,event_seq,agent_id,kind,created_at,data_json)
            VALUES(?1,?2,?3,'fixture','2026-10-01T00:00:00Z','{}')",
        )?;
        for index in 0..=CONTINUITY_MAX_RECORDS {
            insert.execute(params![
                format!("bulk-event-{index}"),
                last + i64::try_from(index)? + 1,
                AGENT
            ])?;
        }
        drop(insert);
        tx.commit()?;
    }
    apply_migration(&mut connection, current_migration())?;
    let acked: usize = connection.query_row(
        "SELECT COUNT(*) FROM event_history_acks WHERE agent_id=?1",
        [AGENT],
        |r| r.get(0),
    )?;
    assert_eq!(acked, original + CONTINUITY_MAX_RECORDS + 1);
    let changed: usize = connection.query_row("SELECT COUNT(*) FROM event_history_acks WHERE agent_id=?1 AND audit_event_id LIKE 'bulk-event-%' AND content_digest!=?2",params![AGENT,bytes_digest(b"{}")],|r|r.get(0))?;
    assert_eq!(changed, 0);
    Ok(())
}

#[test]
fn event_upgrade_pages_preserve_every_ack_including_a_row_larger_than_the_page() -> Result<()> {
    let (mut connection, _) = legacy(0)?;
    migrate_to(&mut connection, current_read_writes::VERSION - 1)?;
    connection.execute_batch(current_migration().sql)?;
    // Isolate the deliberately small byte-page fixture from legacy event framing.
    connection.execute("DELETE FROM audit_events", [])?;
    let scope = InputEvidenceScope {
        agent_id: AGENT.into(),
        identity_incarnation: 1,
    };
    {
        let tx = connection.transaction()?;
        for index in 0..5 {
            tx.execute("INSERT INTO audit_events(audit_event_id,event_seq,agent_id,kind,created_at,data_json)
                VALUES(?1,?2,?3,'fixture','2026-10-01T00:00:00Z',?4)",params![format!("byte-event-{index}"),100+index,AGENT,serde_json::json!({"padding":"x".repeat(20)}).to_string()])?;
        }
        capture_event_history_tx(&tx, &scope, 2, 64)?;
        let events: usize = tx.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE agent_id=?1",
            [AGENT],
            |r| r.get(0),
        )?;
        let acks: usize = tx.query_row(
            "SELECT COUNT(*) FROM event_history_acks WHERE agent_id=?1",
            [AGENT],
            |r| r.get(0),
        )?;
        assert_eq!(events, acks);
        tx.commit()?;
    }
    let before: usize =
        connection.query_row("SELECT COUNT(*) FROM event_history_acks", [], |r| r.get(0))?;
    {
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT INTO audit_events(audit_event_id,event_seq,agent_id,kind,created_at,data_json)
            VALUES('oversized-event',200,?1,'fixture','2026-10-01T00:00:00Z',?2)",
            params![AGENT, "x".repeat(65)],
        )?;
        capture_event_history_tx(&tx, &scope, 2, 64)?;
        let large_ack: usize = tx.query_row(
            "SELECT COUNT(*) FROM event_history_acks WHERE audit_event_id='oversized-event'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(large_ack, 1);
        tx.commit()?;
    }
    let after: usize =
        connection.query_row("SELECT COUNT(*) FROM event_history_acks", [], |r| r.get(0))?;
    assert_eq!(before + 1, after);
    {
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT INTO audit_events(audit_event_id,event_seq,agent_id,kind,created_at,data_json)
            VALUES('invalid-sequence',0,?1,'fixture','2026-10-01T00:00:00Z','{}')",
            [AGENT],
        )?;
        assert!(capture_event_history_tx(&tx, &scope, 2, 64).is_err());
    }
    Ok(())
}
