use super::*;
use crate::{
    runtime_db::RuntimeDb,
    types::{
        brief_created_event_for, AuthorityClass, BriefKind, MessageBody, MessageKind,
        MessageOrigin, Priority, ToolExecutionStatus, TranscriptEntryKind, TurnRecord,
        TurnReplayProvenance,
    },
};
use tempfile::TempDir;

fn database() -> Result<(TempDir, RuntimeDb)> {
    let directory = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )?;
    Ok((directory, db))
}

fn message(id: &str, text: &str) -> MessageEnvelope {
    let mut value = MessageEnvelope::new(
        "agent-a",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text: text.into() },
    );
    value.id = id.into();
    value
}

fn tool() -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: "tool-a".into(),
        agent_id: "agent-a".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: None,
        tool_name: "test".into(),
        created_at: Utc::now(),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Deferred,
        input: serde_json::json!({}),
        output: serde_json::json!({}),
        summary: "pending".into(),
        invocation_surface: None,
    }
}

#[test]
fn standalone_append_returns_allocated_sequence_and_sql_retained_payload() -> Result<()> {
    let (_directory, db) = database()?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER canonical_transcript AFTER INSERT ON transcript_entries
         BEGIN UPDATE transcript_entries SET payload_json = json_set(payload_json, '$.data.sql', 1)
         WHERE evidence_id = NEW.evidence_id; END;",
    )?;
    let entry = TranscriptEntry::new(
        "agent-a",
        TranscriptEntryKind::IncomingMessage,
        None,
        None,
        serde_json::json!({"input": 1}),
    );
    let committed = db.transcript_entries().append(&entry)?;
    assert_eq!(committed.transcript_seq, Some(1));
    assert_eq!(committed.data, serde_json::json!({"input": 1, "sql": 1}));
    assert_eq!(
        Some(committed.clone()),
        db.transcript_entries().by_id(Some("agent-a"), &entry.id)?
    );
    let mut duplicate = entry.clone();
    duplicate.data = serde_json::json!({"retry_candidate": true});
    assert_eq!(db.transcript_entries().append(&duplicate)?, committed);
    let next = TranscriptEntry::new(
        "agent-a",
        TranscriptEntryKind::IncomingMessage,
        None,
        None,
        serde_json::json!({}),
    );
    assert_eq!(
        db.transcript_entries().append(&next)?.transcript_seq,
        Some(2)
    );

    let input = message("message-a", "input");
    let actual = db.messages().append_with_index_changes(&input, &[])?;
    assert_eq!(actual.message_seq, Some(1));
    assert_eq!(
        db.messages().append_with_index_changes(&input, &[])?,
        actual
    );
    Ok(())
}

#[test]
fn standalone_tool_terminal_result_is_actual_row_not_terminal_candidate() -> Result<()> {
    let (_directory, db) = database()?;
    let pending = tool();
    let pending = db.evidence().append_tool_execution_committed(&pending)?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER retain_deferred BEFORE UPDATE ON tool_executions
         BEGIN SELECT RAISE(IGNORE); END;",
    )?;
    let mut terminal = pending.clone();
    terminal.status = ToolExecutionStatus::Success;
    terminal.completed_at = Some(Utc::now());
    terminal.output = serde_json::json!({"ok": true});
    terminal.summary = "done".into();
    assert_eq!(
        db.evidence().append_tool_execution_committed(&terminal)?,
        pending
    );
    db.connection()?
        .execute_batch("DROP TRIGGER retain_deferred")?;
    let committed = db.evidence().append_tool_execution_committed(&terminal)?;
    assert_eq!(committed, terminal);
    assert_eq!(
        Some(committed),
        db.evidence().tool_execution_by_id("agent-a", "tool-a")?
    );
    Ok(())
}

#[test]
fn standalone_brief_linkage_is_exact_and_projection_mismatch_rolls_back() -> Result<()> {
    let (_directory, db) = database()?;
    let brief = BriefRecord::new("agent-a", BriefKind::Result, "done", None, None);
    let event = brief_created_event_for(&brief)?;
    let committed =
        db.evidence()
            .append_brief_with_created_event(Some("agent-a"), &brief, &event, &[])?;
    assert_eq!(
        committed.brief.created_event_seq,
        Some(committed.event.event_seq)
    );
    assert_eq!(
        Some(committed.brief.clone()),
        db.evidence().brief_by_id("agent-a", &brief.id)?
    );
    let replay =
        db.evidence()
            .append_brief_with_created_event(Some("agent-a"), &brief, &event, &[])?;
    assert_eq!(replay.brief, committed.brief);
    assert_eq!(replay.event, committed.event);
    db.connection()?.execute_batch(
        "CREATE TRIGGER corrupt_brief_projection AFTER UPDATE OF created_event_seq ON briefs
         BEGIN UPDATE briefs SET payload_json = json_set(payload_json, '$.created_event_seq', 999999)
         WHERE evidence_id = NEW.evidence_id; END;",
    )?;
    let other = BriefRecord::new("agent-a", BriefKind::Result, "other", None, None);
    let other_event = brief_created_event_for(&other)?;
    let error = db
        .evidence()
        .append_brief_with_created_event(Some("agent-a"), &other, &other_event, &[])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("payload/event projection mismatch"),
        "{error:#}"
    );
    assert!(db.evidence().brief_by_id("agent-a", &other.id)?.is_none());
    let present: bool = db.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM audit_events WHERE audit_event_id = ?1)",
        [&other_event.id],
        |row| row.get(0),
    )?;
    assert!(!present);
    Ok(())
}

#[test]
fn standalone_turn_result_preserves_replay_source_assignment() -> Result<()> {
    let (_directory, db) = database()?;
    db.messages()
        .append_with_index_changes(&message("message-a", "input"), &[])?;
    let mut source = TurnRecord::new("agent-a", "turn-source", 1);
    source.input_message_ids = vec!["message-a".into()];
    let source = db.turn_records().upsert_committed(&source)?;
    assert_eq!(source.input_assignments.len(), 1);
    assert_eq!(source.input_assignments[0].turn_id, "turn-source");
    assert_eq!(source.input_assignments[0].revision, 1);
    assert_eq!(source.affected_source_revisions.len(), 1);
    assert_eq!(source.affected_source_revisions[0].revision, 2);
    assert_eq!(source.affected_source_revisions[0].source_id, "message-a");
    assert_eq!(
        source.affected_source_revisions[0].turn_id.as_deref(),
        Some("turn-source")
    );
    assert_eq!(source.affected_turn_revisions.len(), 1);
    assert_eq!(source.affected_turn_revisions[0].revision, source.revision);
    let mut replay = TurnRecord::new("agent-a", "turn-replay", 2);
    replay.input_message_ids = vec!["message-a".into()];
    replay.replay = Some(TurnReplayProvenance {
        source_message_id: "message-a".into(),
        source_turn_id: "turn-source".into(),
        reason: "restart".into(),
        prior_terminal: None,
    });
    let actual = db.turn_records().upsert_committed(&replay)?;
    assert_eq!(actual.record.turn_id, "turn-replay");
    assert_eq!(actual.input_assignments, source.input_assignments);
    assert_eq!(
        actual.affected_source_revisions,
        source.affected_source_revisions
    );
    assert_eq!(actual.affected_turn_revisions.len(), 2);
    assert_eq!(actual.affected_turn_revisions[0].turn_id, "turn-replay");
    assert_eq!(actual.affected_turn_revisions[0].revision, actual.revision);
    assert_eq!(
        actual.affected_turn_revisions[1],
        source.affected_turn_revisions[0]
    );
    assert!(actual.revision.summary > 0);
    assert_eq!(db.turn_records().upsert_committed(&replay)?, actual);
    Ok(())
}

#[test]
fn standalone_replay_first_assignment_returns_source_turn_projection() -> Result<()> {
    let (_directory, db) = database()?;
    db.messages()
        .append_with_index_changes(&message("message-a", "input"), &[])?;
    let mut source = TurnRecord::new("agent-a", "turn-source", 1);
    let before = db.turn_records().upsert_committed(&source)?;
    // A legacy source Turn can predate conversation input assignments.
    source.input_message_ids = vec!["message-a".into()];
    db.connection()?.execute(
        "UPDATE turn_records SET payload_json = ?2 WHERE turn_id = ?1",
        rusqlite::params![source.turn_id, serde_json::to_string(&source)?],
    )?;
    let mut replay = TurnRecord::new("agent-a", "turn-replay", 2);
    replay.input_message_ids = vec!["message-a".into(), "message-a".into()];
    replay.replay = Some(TurnReplayProvenance {
        source_message_id: "message-a".into(),
        source_turn_id: "turn-source".into(),
        reason: "restart".into(),
        prior_terminal: None,
    });
    let actual = db.turn_records().upsert_committed(&replay)?;
    assert_eq!(actual.input_assignments.len(), 1);
    assert_eq!(actual.input_assignments[0].turn_id, "turn-source");
    assert_eq!(actual.input_assignments[0].revision, 1);
    assert_eq!(actual.affected_source_revisions.len(), 1);
    assert_eq!(actual.affected_source_revisions[0].revision, 2);
    assert_eq!(actual.affected_turn_revisions.len(), 2);
    let source_projection = actual
        .affected_turn_revisions
        .iter()
        .find(|projection| projection.turn_id == "turn-source")
        .unwrap();
    assert_eq!(
        source_projection.revision.summary,
        before.revision.summary + 1
    );
    assert_eq!(
        source_projection.revision.detail,
        before.revision.detail + 1
    );
    let durable_projection: (i64, i64, String) = db.connection()?.query_row(
        "SELECT summary_revision, detail_revision, updated_at FROM conversation_turn_revisions
         WHERE agent_id = 'agent-a' AND turn_id = 'turn-source'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        source_projection.revision.summary,
        u64::try_from(durable_projection.0)?
    );
    assert_eq!(
        source_projection.revision.detail,
        u64::try_from(durable_projection.1)?
    );
    assert_eq!(
        source_projection.updated_at,
        crate::runtime_db::repositories::parse_timestamp(&durable_projection.2)?
    );
    assert_eq!(db.turn_records().upsert_committed(&replay)?, actual);
    Ok(())
}

#[test]
fn standalone_turn_projection_scope_mismatch_rolls_back_canonical_write() -> Result<()> {
    let (_directory, db) = database()?;
    db.messages()
        .append_with_index_changes(&message("message-a", "input"), &[])?;
    db.connection()?.execute_batch(
        "CREATE TRIGGER corrupt_assigned_source AFTER UPDATE OF turn_id ON conversation_source_revisions
         BEGIN UPDATE conversation_source_revisions SET turn_id = 'foreign-turn'
         WHERE source_kind = NEW.source_kind AND source_id = NEW.source_id; END;",
    )?;
    let mut turn = TurnRecord::new("agent-a", "turn-a", 1);
    turn.input_message_ids = vec!["message-a".into()];
    let error = db.turn_records().upsert_committed(&turn).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source projection scope mismatch"),
        "{error:#}"
    );
    assert!(db
        .turn_records()
        .by_id(Some("agent-a"), "turn-a")?
        .is_none());
    let (assignments, source_revision, source_turn): (i64, i64, Option<String>) = db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM conversation_input_assignments WHERE message_id = 'message-a'),
                revision, turn_id FROM conversation_source_revisions
         WHERE source_kind = 'operator' AND source_id = 'message-a'", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(assignments, 0);
    assert_eq!(source_revision, 1);
    assert_eq!(source_turn, None);
    Ok(())
}

#[test]
fn standalone_many_upserts_return_one_final_canonical_fact_per_id() -> Result<()> {
    let (_directory, db) = database()?;
    let first = message("message-a", "first");
    let mut second = first.clone();
    second.body = MessageBody::Text {
        text: "second".into(),
    };
    let result = db
        .messages()
        .upsert_many_committed(&[first, second.clone()])?;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], second);
    Ok(())
}
