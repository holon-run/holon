use super::*;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
    AuthorityClass, BriefContentSource, BriefContentSourceRelation, BriefKind, MessageBody,
    MessageKind, MessageOrigin, Priority, ToolExecutionStatus, TranscriptEntryKind, TurnOwner,
    WorkItemState,
};
use chrono::{DateTime, Utc};

fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    db.agent_identities().upsert(&identity())?;
    let mut state = AgentState::new("cold-fixture");
    state.status = AgentStatus::Stopped;
    db.agent_states().upsert_for_fixture(&state)?;
    Ok((dir, db))
}

fn result_head(db: &RuntimeDb) -> Result<serde_json::Value> {
    let payload: String = db.connection()?.query_row(
        "SELECT head_json FROM work_domain_result_heads WHERE agent_id='cold-fixture' AND work_item_id='cold-work'",
        [], |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&payload)?)
}

fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        "cold-fixture",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}

fn at() -> DateTime<Utc> {
    "2024-01-02T03:04:05.000Z".parse().unwrap()
}

fn message(sequence: u64) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "cold-fixture",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("synthetic".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: format!("synthetic observation {sequence}"),
        },
    );
    message.id = format!("cold-message-{sequence}");
    message.message_seq = Some(sequence);
    message.created_at = at() + chrono::Duration::milliseconds(sequence as i64);
    message
}

fn count(db: &RuntimeDb, table: &str) -> Result<usize> {
    Ok(db
        .connection()?
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })?)
}

#[test]
fn full_cold_combination_preserves_qualification_birth_queue_and_result_head() -> Result<()> {
    let (_dir, db) = fixture()?;
    let writer = ColdFixtureWriter::new(&db, "cold-fixture")?;
    let mut work = WorkItemRecord::new("cold-fixture", "bounded fixture", WorkItemState::Open);
    work.id = "cold-work".into();
    writer.insert_work(&work)?;
    let mut history = message(1);
    history.work_item_id = Some(work.id.clone());
    history.turn_id = Some("cold-turn".into());
    writer.insert(ColdFixtureEvidence::Message(&history))?;
    let mut brief = BriefRecord::new(
        "cold-fixture",
        BriefKind::Result,
        "verified",
        Some(history.id.clone()),
        None,
    );
    brief.id = "cold-brief".into();
    brief.turn_id = history.turn_id.clone();
    brief.turn_index = Some(1);
    brief.work_item_id = Some(work.id.clone());
    brief.content_source = BriefContentSource::TranscriptEntry {
        entry_id: "cold-transcript".into(),
        relation: BriefContentSourceRelation::Finalizes,
    };
    writer.insert(ColdFixtureEvidence::Brief(&brief))?;
    assert_eq!(result_head(&db)?["brief_id"], brief.id);
    assert!(result_head(&db)?["transcript_entry_id"].is_null());
    let tool = ToolExecutionRecord {
        id: "cold-tool".into(),
        agent_id: "cold-fixture".into(),
        work_item_id: Some(work.id.clone()),
        turn_index: 1,
        turn_id: history.turn_id.clone(),
        tool_name: "ExecCommand".into(),
        created_at: at(),
        completed_at: Some(at()),
        duration_ms: 1,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::json!({"cmd":"synthetic"}),
        output: serde_json::json!({"stdout":"x".repeat(1024 * 1024)}),
        summary: "bounded summary".into(),
        invocation_surface: Some("commentary".into()),
    };
    writer.insert(ColdFixtureEvidence::Tool(&tool))?;
    let mut turn = TurnRecord::new("cold-fixture", "cold-turn", 1);
    turn.created_at = at();
    turn.current_work_item_id = Some(work.id.clone());
    turn.owner = Some(TurnOwner::WorkItem {
        work_item_id: work.id.clone(),
    });
    turn.input_message_ids.push(history.id.clone());
    turn.produced_brief_ids.push(brief.id.clone());
    turn.tool_execution_ids.push(tool.id.clone());
    writer.insert(ColdFixtureEvidence::Turn(&turn))?;
    let mut transcript = TranscriptEntry::new(
        "cold-fixture",
        TranscriptEntryKind::AssistantRound,
        Some(1),
        Some(history.id.clone()),
        serde_json::json!({"turn_id":"cold-turn","blocks":[{"type":"text","text":"verified"}]}),
    );
    transcript.id = "cold-transcript".into();
    transcript.created_at = at();
    writer.insert(ColdFixtureEvidence::Transcript(&transcript))?;
    let pending = message(2);
    let queue = QueueEntryRecord {
        agent_id: "cold-fixture".into(),
        message_id: pending.id.clone(),
        priority: pending.priority.clone(),
        status: QueueEntryStatus::Queued,
        created_at: pending.created_at,
        updated_at: pending.created_at,
    };
    writer.insert_pending(&pending, &queue)?;
    let mut state = AgentState::new("cold-fixture");
    state.status = AgentStatus::Stopped;
    state.current_work_item_id = Some(work.id.clone());
    state.turn_index = 1;
    state.total_message_count = 2;
    state.pending = 1;
    writer.finish_state(&state)?;

    let connection = db.connection()?;
    let qualified: usize = connection.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE agent_id='cold-fixture'
         AND identity_incarnation=1 AND qualification='committed_writer' AND availability='present'",
        [], |row| row.get(0),
    )?;
    assert_eq!(qualified, 6);
    assert_eq!(count(&db, "input_query_birth_members")?, 6);
    assert_eq!(count(&db, "current_read_qualification")?, 1);
    let head = result_head(&db)?;
    assert_eq!(head["brief_id"], brief.id);
    assert_eq!(head["transcript_entry_id"], transcript.id);
    let facts = db.transaction(|tx| {
        work_snapshot::prepare_work_domain_snapshot_tx(tx, &writer.scope, &work_request())?
            .require_current()?;
        Ok(())
    });
    facts?;
    assert_eq!(db.agent_states().latest("cold-fixture")?.unwrap(), state);
    Ok(())
}

#[test]
fn bulk_rows_are_identical_to_canonical_original_inserts_across_batch_boundary() -> Result<()> {
    let (_left, canonical) = fixture()?;
    let (_right, bulk) = fixture()?;
    let canonical_writer = ColdFixtureWriter::new(&canonical, "cold-fixture")?;
    let bulk_writer = ColdFixtureWriter::new(&bulk, "cold-fixture")?;
    let messages = (1..=129).map(message).collect::<Vec<_>>();
    for message in &messages {
        canonical_writer.insert(ColdFixtureEvidence::Message(message))?;
    }
    bulk_writer.insert(ColdFixtureEvidence::Message(&messages[0]))?;
    bulk_writer.insert_unassigned_messages(&messages[1..])?;
    for table in [
        "messages",
        "conversation_source_revisions",
        "runtime_sequences",
        "input_evidence_current",
        "input_query_birth_members",
        "input_query_birth_cursors",
    ] {
        fn rows(db: &RuntimeDb, table: &str) -> Result<Vec<Vec<rusqlite::types::Value>>> {
            let connection = db.connection()?;
            let mut statement =
                connection.prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))?;
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get(column))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }
        // Qualification timestamps/revisions are writer observations, not fixture bodies.
        if table == "input_evidence_current" {
            let qualified = count(&bulk, table)?;
            assert_eq!(qualified, count(&canonical, table)?);
            assert_eq!(qualified, messages.len());
        } else {
            assert_eq!(rows(&canonical, table)?, rows(&bulk, table)?, "{table}");
        }
    }
    assert_eq!(bulk.messages().max_message_seq(Some("cold-fixture"))?, 129);
    assert_eq!(
        bulk.agent_states()
            .latest("cold-fixture")?
            .unwrap()
            .total_message_count,
        129
    );
    Ok(())
}

#[test]
fn failed_original_batch_rolls_back_rows_sources_sequences_aliases_and_births() -> Result<()> {
    let (_dir, db) = fixture()?;
    let writer = ColdFixtureWriter::new(&db, "cold-fixture")?;
    let before = [
        "messages",
        "conversation_source_revisions",
        "runtime_sequences",
        "input_evidence_current",
        "input_query_birth_members",
        "input_query_birth_cursors",
    ];
    let counts = before
        .iter()
        .map(|table| count(&db, table))
        .collect::<Result<Vec<_>>>()?;
    let error = writer
        .insert_unassigned_messages(&[message(1), message(3)])
        .unwrap_err();
    assert!(
        error.to_string().contains("sequence is not canonical"),
        "{error:#}"
    );
    for (table, expected) in before.iter().zip(counts) {
        assert_eq!(count(&db, table)?, expected, "{table}");
    }
    writer.insert_unassigned_messages(&[message(1), message(2)])?;
    assert_eq!(count(&db, "messages")?, 2);
    Ok(())
}

#[test]
fn cold_writer_rejects_raw_late_rows_foreign_live_and_retired_scopes() -> Result<()> {
    let (_dir, db) = fixture()?;
    let writer = ColdFixtureWriter::new(&db, "cold-fixture")?;
    let raw = message(1);
    db.messages().upsert(&raw)?;
    assert!(writer
        .insert(ColdFixtureEvidence::Message(&raw))
        .unwrap_err()
        .to_string()
        .contains("cannot adopt existing"));
    assert_eq!(count(&db, "input_evidence_current")?, 0);
    assert_eq!(count(&db, "input_query_birth_members")?, 0);
    let mut foreign = message(2);
    foreign.agent_id = "other-agent".into();
    assert!(writer
        .insert(ColdFixtureEvidence::Message(&foreign))
        .is_err());
    let raw_work = WorkItemRecord::new("cold-fixture", "raw late", WorkItemState::Open);
    db.work_items().insert_new(&raw_work)?;
    assert!(writer.insert_work(&raw_work).is_err());
    assert_eq!(count(&db, "work_domain_records")?, 0);
    let mut state = db.agent_states().latest("cold-fixture")?.unwrap();
    state.status = AgentStatus::AwakeIdle;
    db.agent_states().upsert_for_fixture(&state)?;
    assert!(ColdFixtureWriter::new(&db, "cold-fixture").is_err());
    assert!(writer
        .insert(ColdFixtureEvidence::Message(&message(2)))
        .is_err());
    state.status = AgentStatus::Stopped;
    db.agent_states().upsert_for_fixture(&state)?;
    let mut next = identity();
    next.incarnation = 2;
    db.agent_identities().upsert(&next)?;
    assert!(writer
        .insert(ColdFixtureEvidence::Message(&message(2)))
        .is_err());
    assert_eq!(count(&db, "messages")?, 1);
    Ok(())
}
