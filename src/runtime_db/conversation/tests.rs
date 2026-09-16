use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use anyhow::Result;
use chrono::{Duration, TimeZone, Utc};
use rusqlite::params;
use tempfile::TempDir;

use super::{
    collect_change_ids, settle_turn_result_tx, ConversationReadError, ConversationResetReason,
    CONVERSATION_ACTIVITY_BEFORE_SQL, CONVERSATION_HISTORY_BEFORE_SQL, MAX_BRIEFS_PER_TURN,
    MAX_CHANGE_ID_JSON_DEPTH, MAX_CONVERSATION_CHANGE_ACTIVITIES,
};
use crate::domain::conversation::{
    ActivityItem, Attention, ConversationActivity, ConversationChange,
    ConversationShadowMismatchKind, CursorBinding, CursorCodec, ExecutionState, NoBriefReason,
    PendingInputState, ResultState, StreamCursor, TerminalOutcome, TurnKey,
    CONVERSATION_QUERY_VERSION, CONVERSATION_SCHEMA_VERSION,
};
use crate::runtime_db::{migrations::CONVERSATION_REPLAY_INPUT_SOURCE_SELECT_SQL, RuntimeDb};
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
    AgentVisibility, AuditEvent, AuthorityClass, BriefKind, BriefRecord, ContinuationTriggerKind,
    MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, QueueEntryRecord,
    QueueEntryStatus, ToolExecutionRecord, ToolExecutionStatus, TranscriptEntry,
    TranscriptEntryKind, TurnNoBriefReason, TurnRecord, TurnReplayProvenance, TurnTerminalKind,
    TurnTerminalSummary, TurnTriggerSummary,
};

const AGENT_ID: &str = "agent-conversation-test";

fn runtime_db() -> Result<(TempDir, PathBuf, PathBuf, RuntimeDb)> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("state/runtime.sqlite");
    let lock_path = temp_dir.path().join("state/runtime.lock");
    std::fs::create_dir_all(db_path.parent().expect("database parent"))?;
    let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    Ok((temp_dir, db_path, lock_path, db))
}

fn overwrite_turn_payload(db: &RuntimeDb, turn: &TurnRecord) -> Result<()> {
    db.connection()?.execute(
        "UPDATE turn_records SET payload_json = ?1 WHERE turn_id = ?2",
        params![serde_json::to_string(turn)?, turn.turn_id],
    )?;
    Ok(())
}

fn register_public_agent(db: &RuntimeDb) -> Result<()> {
    let mut identity = AgentIdentityRecord::new(
        AGENT_ID,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    identity.status = AgentRegistryStatus::Active;
    identity.created_at = timestamp(0);
    identity.updated_at = timestamp(0);
    db.agent_identities().upsert(&identity)
}

fn append_turn_event(db: &RuntimeDb, event_id: &str, turn_id: &str, offset: i64) -> Result<u64> {
    let mut event = AuditEvent::legacy(
        "conversation_test_change",
        serde_json::json!({ "turn_id": turn_id }),
    );
    event.id = event_id.into();
    event.created_at = timestamp(offset);
    Ok(db.audit_events().append(Some(AGENT_ID), &event)?.event_seq)
}

fn timestamp(offset: i64) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0)
        .single()
        .expect("valid timestamp")
        + Duration::seconds(offset)
}

fn trigger(kind: MessageKind) -> TurnTriggerSummary {
    TurnTriggerSummary {
        message_id: Some(format!("trigger-{kind:?}")),
        kind,
        origin: MessageOrigin::System {
            subsystem: "conversation-test".into(),
        },
        authority_class: AuthorityClass::RuntimeInstruction,
        priority: Priority::Normal,
        trigger_kind: Some(ContinuationTriggerKind::SystemTick),
        task_id: None,
    }
}

fn turn(turn_id: &str, turn_index: u64) -> TurnRecord {
    let mut record = TurnRecord::new(AGENT_ID, turn_id, turn_index);
    record.created_at = timestamp(i64::try_from(turn_index).expect("test turn index"));
    record.trigger = Some(trigger(MessageKind::OperatorPrompt));
    record
}

fn terminal(
    mut record: TurnRecord,
    kind: TurnTerminalKind,
    no_brief_reason: Option<TurnNoBriefReason>,
) -> TurnRecord {
    record.terminal = Some(TurnTerminalSummary {
        kind,
        reason: None,
        no_brief_reason,
        completed_at: record.created_at + Duration::seconds(1),
        duration_ms: 1_000,
    });
    record
}

fn activity_item(activity: &ConversationActivity) -> &ActivityItem {
    match activity {
        ConversationActivity::Operator(item)
        | ConversationActivity::Assistant(item)
        | ConversationActivity::Tool(item)
        | ConversationActivity::Wait(item)
        | ConversationActivity::Error(item) => item,
    }
}

#[test]
fn summary_snapshot_keeps_records_and_event_head_on_one_concurrent_read_view() -> Result<()> {
    let (_temp_dir, db_path, lock_path, db) = runtime_db()?;
    let mut identity = AgentIdentityRecord::new(
        AGENT_ID,
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    identity.status = AgentRegistryStatus::Active;
    identity.created_at = timestamp(0);
    identity.updated_at = timestamp(0);
    db.agent_identities().upsert(&identity)?;
    db.turn_records().upsert(&terminal(
        turn("turn-concurrent-snapshot", 1),
        TurnTerminalKind::Completed,
        None,
    ))?;

    let baseline = db
        .conversation()
        .summary_snapshot(AGENT_ID, 10, None, "test-principal", "public")?
        .expect("baseline snapshot");
    assert!(baseline.value.turns[0].brief_ids.is_empty());

    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = Arc::clone(&barrier);
    let writer = std::thread::spawn(move || -> Result<()> {
        let writer_db = RuntimeDb::open_and_migrate(db_path, lock_path)?;
        let mut brief = BriefRecord::new(AGENT_ID, BriefKind::Result, "late result", None, None);
        brief.id = "brief-concurrent-snapshot".into();
        brief.turn_id = Some("turn-concurrent-snapshot".into());
        brief.turn_index = Some(1);
        brief.created_at = timestamp(10);
        let event = crate::types::brief_created_event_for(&brief)?;
        writer_barrier.wait();
        writer_db.evidence().append_brief_with_created_event(
            Some(AGENT_ID),
            &brief,
            &event,
            &[],
        )?;
        writer_barrier.wait();
        Ok(())
    });

    let during_write = db
        .conversation()
        .summary_snapshot_after_context(AGENT_ID, 10, None, "test-principal", "public", || {
            barrier.wait();
            barrier.wait();
        })?
        .expect("snapshot during write");
    writer.join().expect("brief writer")?;

    assert_eq!(during_write.event_head_seq, baseline.event_head_seq);
    assert!(during_write.value.turns[0].brief_ids.is_empty());
    assert_eq!(during_write.value.turns[0].result, ResultState::Pending);

    let after_write = db
        .conversation()
        .summary_snapshot(AGENT_ID, 10, None, "test-principal", "public")?
        .expect("snapshot after write");
    assert!(after_write.event_head_seq > during_write.event_head_seq);
    assert_eq!(
        after_write.value.turns[0].brief_ids,
        ["brief-concurrent-snapshot"]
    );
    assert_eq!(after_write.value.turns[0].result, ResultState::Available);
    Ok(())
}

#[test]
fn summary_rejects_unbounded_brief_membership() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    db.turn_records().upsert(&terminal(
        turn("turn-many-briefs", 1),
        TurnTerminalKind::Completed,
        None,
    ))?;
    for index in 0..=MAX_BRIEFS_PER_TURN {
        let mut brief = BriefRecord::new(
            AGENT_ID,
            BriefKind::Result,
            format!("brief {index}"),
            None,
            None,
        );
        brief.id = format!("brief-{index:03}");
        brief.turn_id = Some("turn-many-briefs".into());
        brief.turn_index = Some(1);
        brief.created_at = timestamp(i64::try_from(index).unwrap());
        db.evidence().append_brief(&brief)?;
    }

    let error = db
        .conversation()
        .summary_page(AGENT_ID, 10, None, None)
        .expect_err("brief membership must remain bounded");
    assert_eq!(
        error.downcast_ref::<super::ConversationReadError>(),
        Some(&super::ConversationReadError::CountLimitExceeded {
            resource: "briefs per turn",
            limit: MAX_BRIEFS_PER_TURN,
        })
    );
    Ok(())
}

#[test]
fn shadow_diagnostics_compare_bounded_metadata_and_report_legacy_briefs() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    register_public_agent(&db)?;
    db.turn_records().upsert(&terminal(
        turn("turn-shadow", 1),
        TurnTerminalKind::Completed,
        None,
    ))?;

    let mut brief = BriefRecord::new(
        AGENT_ID,
        BriefKind::Result,
        "linked body must not appear in diagnostics",
        None,
        None,
    );
    brief.id = "brief-shadow".into();
    brief.turn_id = Some("turn-shadow".into());
    brief.turn_index = Some(1);
    brief.created_at = timestamp(10);
    db.evidence().append_brief(&brief)?;

    let mut legacy = BriefRecord::new(
        AGENT_ID,
        BriefKind::Result,
        "legacy body must not appear in diagnostics",
        None,
        None,
    );
    legacy.id = "brief-shadow-legacy".into();
    legacy.created_at = timestamp(11);
    db.evidence().append_brief(&legacy)?;

    let report = db
        .conversation()
        .shadow_diagnostics(AGENT_ID, 10, "test-principal", "public")?
        .expect("shadow diagnostics");
    assert_eq!(report.checked_turn_limit, 10);
    assert_eq!(report.canonical, report.projection);
    assert_eq!(report.canonical.turns, 1);
    assert_eq!(report.canonical.briefs, 1);
    assert_eq!(report.legacy_unattributed_briefs, 1);
    assert_eq!(report.mismatch_count, 0);
    assert!(report.mismatches.is_empty());
    let encoded = serde_json::to_string(&report)?;
    assert!(!encoded.contains("linked body"));
    assert!(!encoded.contains("legacy body"));

    db.connection()?.execute(
        "DELETE FROM conversation_turn_revisions
         WHERE agent_id = ?1 AND turn_id = 'turn-shadow'",
        [AGENT_ID],
    )?;
    let drifted = db
        .conversation()
        .shadow_diagnostics(AGENT_ID, 10, "test-principal", "public")?
        .expect("drifted shadow diagnostics");
    assert_eq!(drifted.mismatch_count, 1);
    assert_eq!(
        drifted.mismatches[0].kind,
        ConversationShadowMismatchKind::TurnRevision
    );
    assert_eq!(drifted.mismatches[0].entity_id, "turn-shadow");
    assert_eq!(drifted.mismatches[0].canonical_revision, None);
    assert_eq!(drifted.mismatches[0].projection_revision, Some(1));
    Ok(())
}

#[test]
fn history_keyset_preserves_upper_bound_and_legacy_ties() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    for record in [
        terminal(
            turn("turn-a", 1),
            TurnTerminalKind::Completed,
            Some(TurnNoBriefReason::ToolOnlyWait),
        ),
        terminal(
            turn("turn-b", 1),
            TurnTerminalKind::Completed,
            Some(TurnNoBriefReason::ToolOnlyWait),
        ),
        terminal(
            turn("turn-c", 2),
            TurnTerminalKind::Completed,
            Some(TurnNoBriefReason::ToolOnlyWait),
        ),
    ] {
        db.turn_records().upsert(&record)?;
    }

    let first = db.conversation().summary_page(AGENT_ID, 2, None, None)?;
    assert_eq!(
        first
            .turns
            .iter()
            .map(|turn| turn.turn_id.as_str())
            .collect::<Vec<_>>(),
        ["turn-b", "turn-c"]
    );
    assert_eq!(
        first.membership_upper_bound,
        Some(TurnKey {
            turn_index: 2,
            turn_id: "turn-c".into(),
        })
    );
    assert_eq!(
        first.next_before,
        Some(TurnKey {
            turn_index: 1,
            turn_id: "turn-b".into(),
        })
    );
    assert!(first.has_more);

    db.turn_records().upsert(&terminal(
        turn("turn-new", 3),
        TurnTerminalKind::Completed,
        Some(TurnNoBriefReason::ToolOnlyWait),
    ))?;

    let older = db.conversation().summary_page(
        AGENT_ID,
        2,
        first.next_before.as_ref(),
        first.membership_upper_bound.as_ref(),
    )?;
    assert_eq!(
        older
            .turns
            .iter()
            .map(|turn| turn.turn_id.as_str())
            .collect::<Vec<_>>(),
        ["turn-a"]
    );
    assert!(!older.has_more);
    Ok(())
}

#[test]
fn summary_includes_assigned_input_previews() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    let mut message = MessageEnvelope::new(
        AGENT_ID,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator-test".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "summarize the release".into(),
        },
    );
    message.id = "message-input-preview".into();
    message.turn_id = Some("turn-input-preview".into());
    message.created_at = timestamp(1);
    db.evidence().append_message(&message)?;

    let mut source = turn("turn-input-preview", 1);
    source.trigger = Some(TurnTriggerSummary::from_message(&message));
    source.input_message_ids = vec![message.id.clone()];
    db.turn_records()
        .upsert(&terminal(source, TurnTerminalKind::Completed, None))?;

    let page = db.conversation().summary_page(AGENT_ID, 10, None, None)?;
    let summary = page
        .turns
        .iter()
        .find(|turn| turn.turn_id == "turn-input-preview")
        .expect("turn summary");
    assert_eq!(summary.inputs.len(), 1);
    assert_eq!(summary.inputs[0].message_id, "message-input-preview");
    assert!(
        summary.inputs[0].preview.contains("summarize the release"),
        "preview should carry the input text: {}",
        summary.inputs[0].preview
    );
    Ok(())
}

#[test]
fn replayed_input_keeps_source_turn_assignment() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    let mut message = MessageEnvelope::new(
        AGENT_ID,
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "resume work item".into(),
        },
    );
    message.id = "message-replay-source".into();
    message.turn_id = Some("turn-replay-source".into());
    message.created_at = timestamp(1);
    db.evidence().append_message(&message)?;

    let mut source = turn("turn-replay-source", 1);
    source.trigger = Some(TurnTriggerSummary::from_message(&message));
    source.input_message_ids = vec![message.id.clone()];
    source = terminal(
        source,
        TurnTerminalKind::Aborted,
        Some(TurnNoBriefReason::Aborted),
    );
    db.turn_records().upsert(&source)?;

    let mut replay = turn("turn-replay-attempt", 2);
    replay.trigger = Some(TurnTriggerSummary::from_message(&message));
    replay.input_message_ids = vec![message.id.clone()];
    replay.replay = Some(TurnReplayProvenance {
        source_message_id: message.id.clone(),
        source_turn_id: source.turn_id.clone(),
        reason: "interrupted_queue_claim_reentry".into(),
        prior_terminal: source.terminal.clone(),
    });
    db.turn_records().upsert(&replay)?;

    let connection = db.connection()?;
    let assignment = connection.query_row(
        "SELECT turn_id, revision
         FROM conversation_input_assignments
         WHERE message_id = ?1",
        [&message.id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
    )?;
    assert_eq!(assignment, (source.turn_id, 1));
    Ok(())
}

#[test]
fn legacy_schema_migration_backfills_visible_activity_sequences() -> Result<()> {
    let (_temp_dir, db_path, lock_path, db) = runtime_db()?;
    let mut message = MessageEnvelope::new(
        AGENT_ID,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "legacy operator input".into(),
        },
    );
    message.id = "legacy-message".into();
    message.created_at = timestamp(1);
    db.evidence().append_message(&message)?;

    let mut record = turn("legacy-turn", 1);
    record.input_message_ids = vec![message.id.clone()];
    record.tool_execution_ids = vec!["legacy-tool".into()];
    let mut replay = turn("legacy-replay", 2);
    replay.input_message_ids = vec![message.id.clone()];
    replay.replay = Some(TurnReplayProvenance {
        source_message_id: message.id.clone(),
        source_turn_id: record.turn_id.clone(),
        reason: "legacy_replay".into(),
        prior_terminal: None,
    });
    db.turn_records().upsert(&replay)?;
    db.turn_records().upsert(&record)?;

    let mut assistant = TranscriptEntry::new(
        AGENT_ID,
        TranscriptEntryKind::AssistantRound,
        Some(1),
        Some(message.id.clone()),
        serde_json::json!({
            "turn_id": "legacy-turn",
            "text": "legacy assistant activity",
        }),
    );
    assistant.id = "legacy-assistant".into();
    assistant.created_at = timestamp(2);
    db.evidence().append_transcript_entry(&assistant)?;

    db.evidence().append_tool_execution(&ToolExecutionRecord {
        id: "legacy-tool".into(),
        agent_id: AGENT_ID.into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: Some("legacy-turn".into()),
        tool_name: "ExecCommand".into(),
        created_at: timestamp(3),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Deferred,
        input: serde_json::json!({ "cmd": "true" }),
        output: serde_json::Value::Null,
        summary: "legacy tool".into(),
        invocation_surface: None,
    })?;
    drop(db);

    let connection = rusqlite::Connection::open(&db_path)?;
    connection.execute_batch(
        "DROP TABLE conversation_input_assignments;
         DROP TABLE conversation_source_revisions;
         DROP TABLE conversation_turn_revisions;
         DELETE FROM schema_migrations WHERE version = 64;",
    )?;
    drop(connection);

    let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let detail = db
        .conversation()
        .activities(AGENT_ID, "legacy-turn", 10, None, None)?
        .expect("legacy turn");
    assert_eq!(
        detail
            .activities
            .iter()
            .map(|activity| activity_item(activity).id.as_str())
            .collect::<Vec<_>>(),
        [
            "operator:legacy-message",
            "assistant:legacy-assistant",
            "tool:legacy-tool",
        ]
    );
    assert!(detail
        .activities
        .windows(2)
        .all(|pair| activity_item(&pair[0]).key < activity_item(&pair[1]).key));
    let assignment = db.connection()?.query_row(
        "SELECT turn_id
         FROM conversation_input_assignments
         WHERE message_id = ?1",
        [&message.id],
        |row| row.get::<_, String>(0),
    )?;
    assert_eq!(assignment, "legacy-turn");
    Ok(())
}

#[test]
fn migration_repairs_existing_replay_input_assignment() -> Result<()> {
    let (_temp_dir, db_path, lock_path, db) = runtime_db()?;
    let mut source = turn("repair-source", 1);
    source.input_message_ids = vec!["repair-message".into()];
    db.turn_records().upsert(&source)?;
    let mut replay = turn("repair-replay", 2);
    replay.input_message_ids = source.input_message_ids.clone();
    replay.replay = Some(TurnReplayProvenance {
        source_message_id: "repair-message".into(),
        source_turn_id: source.turn_id.clone(),
        reason: "repair_test".into(),
        prior_terminal: None,
    });
    db.turn_records().upsert(&replay)?;
    let stale_replay = turn("repair-stale-replay", 3);
    db.turn_records().upsert(&stale_replay)?;
    let mut stale_replay_payload = stale_replay;
    stale_replay_payload.input_message_ids = source.input_message_ids.clone();
    stale_replay_payload.replay = Some(TurnReplayProvenance {
        source_message_id: "repair-message".into(),
        source_turn_id: "repair-missing-source".into(),
        reason: "repair_test".into(),
        prior_terminal: None,
    });
    overwrite_turn_payload(&db, &stale_replay_payload)?;
    db.connection()?.execute_batch(
        "UPDATE conversation_input_assignments
         SET turn_id = 'repair-replay'
         WHERE message_id = 'repair-message';
         DELETE FROM schema_migrations WHERE version = 66;",
    )?;
    drop(db);

    let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let assignment = db.connection()?.query_row(
        "SELECT turn_id, revision
         FROM conversation_input_assignments
         WHERE message_id = 'repair-message'",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
    )?;
    assert_eq!(assignment, ("repair-source".into(), 2));
    let stale_replay_type: Option<String> = db.connection()?.query_row(
        "SELECT json_type(payload_json, '$.replay')
         FROM turn_records
         WHERE turn_id = 'repair-stale-replay'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(stale_replay_type, None);
    db.connection()?
        .execute("DELETE FROM schema_migrations WHERE version = 66", [])?;
    drop(db);

    let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    let assignment = db.connection()?.query_row(
        "SELECT turn_id, revision
         FROM conversation_input_assignments
         WHERE message_id = 'repair-message'",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
    )?;
    assert_eq!(assignment, ("repair-source".into(), 2));
    Ok(())
}

#[test]
fn migration_replay_scan_query_plan_has_no_correlated_turn_scan() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    let connection = db.connection()?;
    let mut statement = connection.prepare(&format!(
        "EXPLAIN QUERY PLAN {CONVERSATION_REPLAY_INPUT_SOURCE_SELECT_SQL}"
    ))?;
    let details = statement
        .query_map([], |row| row.get::<_, String>(3))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        details
            .iter()
            .filter(|detail| detail.contains("SCAN replay_turn"))
            .count(),
        1,
        "{details:?}"
    );
    assert!(
        details.iter().all(|detail| !detail.contains("CORRELATED")),
        "{details:?}"
    );
    Ok(())
}

#[test]
fn migration_repair_handles_large_non_replay_history_without_candidate_cross_product() -> Result<()>
{
    let (_temp_dir, db_path, lock_path, db) = runtime_db()?;
    db.connection()?.execute_batch(
        r#"
WITH digits(value) AS (
  VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9)
),
sequence(value) AS (
  SELECT thousands.value * 1000
       + hundreds.value * 100
       + tens.value * 10
       + ones.value
       + 1
  FROM digits AS thousands
  CROSS JOIN digits AS hundreds
  CROSS JOIN digits AS tens
  CROSS JOIN digits AS ones
)
INSERT INTO turn_records (
  turn_id, turn_index, agent_id, created_at, payload_json
)
SELECT
  printf('bulk-turn-%05d', value),
  value,
  'bulk-agent',
  '2026-09-15T00:00:00Z',
  json_object(
    'turn_id', printf('bulk-turn-%05d', value),
    'turn_index', value,
    'agent_id', 'bulk-agent',
    'input_message_ids', json_array(printf('bulk-message-%05d', value)),
    'created_at', '2026-09-15T00:00:00Z'
  )
FROM sequence;

INSERT INTO conversation_input_assignments (
  message_id, agent_id, turn_id, revision, assigned_at
)
SELECT
  printf('bulk-message-%05d', turn_index),
  agent_id,
  turn_id,
  1,
  created_at
FROM turn_records
WHERE agent_id = 'bulk-agent';

DELETE FROM schema_migrations WHERE version = 66;
"#,
    )?;
    drop(db);

    let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    assert_eq!(db.current_schema_version()?, 66);
    let assignment_count: i64 = db.connection()?.query_row(
        "SELECT COUNT(*)
         FROM conversation_input_assignments
         WHERE agent_id = 'bulk-agent'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(assignment_count, 10000);
    Ok(())
}

#[test]
fn migration_skips_conflicting_replay_input_sources() -> Result<()> {
    let (_temp_dir, db_path, lock_path, db) = runtime_db()?;
    let mut source_one = turn("repair-source-one", 1);
    source_one.input_message_ids = vec!["repair-conflict-message".into()];
    db.turn_records().upsert(&source_one)?;
    let source_two = turn("repair-source-two", 2);
    db.turn_records().upsert(&source_two)?;
    let replay_one = turn("repair-replay-one", 3);
    db.turn_records().upsert(&replay_one)?;
    let replay_two = turn("repair-replay-two", 4);
    db.turn_records().upsert(&replay_two)?;

    let mut source_two_payload = source_two;
    source_two_payload.input_message_ids = source_one.input_message_ids.clone();
    overwrite_turn_payload(&db, &source_two_payload)?;
    let mut replay_one_payload = replay_one;
    replay_one_payload.input_message_ids = source_one.input_message_ids.clone();
    replay_one_payload.replay = Some(TurnReplayProvenance {
        source_message_id: "repair-conflict-message".into(),
        source_turn_id: source_one.turn_id.clone(),
        reason: "repair_test".into(),
        prior_terminal: None,
    });
    overwrite_turn_payload(&db, &replay_one_payload)?;
    let mut replay_two_payload = replay_two;
    replay_two_payload.input_message_ids = source_one.input_message_ids.clone();
    replay_two_payload.replay = Some(TurnReplayProvenance {
        source_message_id: "repair-conflict-message".into(),
        source_turn_id: source_two_payload.turn_id.clone(),
        reason: "repair_test".into(),
        prior_terminal: None,
    });
    overwrite_turn_payload(&db, &replay_two_payload)?;
    db.connection()?
        .execute("DELETE FROM schema_migrations WHERE version = 66", [])?;
    drop(db);

    let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    assert_eq!(db.current_schema_version()?, 66);
    let assignment = db.connection()?.query_row(
        "SELECT turn_id, revision
         FROM conversation_input_assignments
         WHERE message_id = 'repair-conflict-message'",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
    )?;
    assert_eq!(assignment, ("repair-source-one".into(), 1));
    let replay_count: i64 = db.connection()?.query_row(
        "SELECT COUNT(*)
         FROM turn_records
         WHERE turn_id IN ('repair-replay-one', 'repair-replay-two')
           AND json_type(payload_json, '$.replay') = 'object'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(replay_count, 2);
    Ok(())
}

#[test]
fn migration_discards_invalid_replay_provenance_without_reassigning_inputs() -> Result<()> {
    let (_temp_dir, db_path, lock_path, db) = runtime_db()?;
    db.connection()?.execute_batch(
        r#"
INSERT INTO turn_records (
  turn_id, turn_index, agent_id, created_at, payload_json
) VALUES
(
  'invalid-replay-incomplete', 1, 'agent-conversation-test',
  '2026-09-15T00:00:01Z',
  json_object(
    'turn_id', 'invalid-replay-incomplete',
    'turn_index', 1,
    'agent_id', 'agent-conversation-test',
    'input_message_ids', json_array('invalid-incomplete-message'),
    'replay', json_object(
      'source_message_id', 'invalid-incomplete-message',
      'source_turn_id', ''
    ),
    'created_at', '2026-09-15T00:00:01Z'
  )
),
(
  'invalid-replay-missing-input', 2, 'agent-conversation-test',
  '2026-09-15T00:00:02Z',
  json_object(
    'turn_id', 'invalid-replay-missing-input',
    'turn_index', 2,
    'agent_id', 'agent-conversation-test',
    'input_message_ids', json_array(),
    'replay', json_object(
      'source_message_id', 'invalid-missing-input-message',
      'source_turn_id', 'invalid-source-valid'
    ),
    'created_at', '2026-09-15T00:00:02Z'
  )
),
(
  'invalid-replay-missing-source', 3, 'agent-conversation-test',
  '2026-09-15T00:00:03Z',
  json_object(
    'turn_id', 'invalid-replay-missing-source',
    'turn_index', 3,
    'agent_id', 'agent-conversation-test',
    'input_message_ids', json_array('invalid-missing-source-message'),
    'replay', json_object(
      'source_message_id', 'invalid-missing-source-message',
      'source_turn_id', 'invalid-source-absent'
    ),
    'created_at', '2026-09-15T00:00:03Z'
  )
),
(
  'invalid-replay-cross-agent', 4, 'agent-conversation-test',
  '2026-09-15T00:00:04Z',
  json_object(
    'turn_id', 'invalid-replay-cross-agent',
    'turn_index', 4,
    'agent_id', 'agent-conversation-test',
    'input_message_ids', json_array('invalid-cross-agent-message'),
    'replay', json_object(
      'source_message_id', 'invalid-cross-agent-message',
      'source_turn_id', 'invalid-source-cross-agent'
    ),
    'created_at', '2026-09-15T00:00:04Z'
  )
),
(
  'invalid-replay-source-missing-input', 5, 'agent-conversation-test',
  '2026-09-15T00:00:05Z',
  json_object(
    'turn_id', 'invalid-replay-source-missing-input',
    'turn_index', 5,
    'agent_id', 'agent-conversation-test',
    'input_message_ids', json_array('invalid-source-missing-input-message'),
    'replay', json_object(
      'source_message_id', 'invalid-source-missing-input-message',
      'source_turn_id', 'invalid-source-missing-input'
    ),
    'created_at', '2026-09-15T00:00:05Z'
  )
),
(
  'invalid-source-valid', 6, 'agent-conversation-test',
  '2026-09-15T00:00:06Z',
  json_object(
    'turn_id', 'invalid-source-valid',
    'turn_index', 6,
    'agent_id', 'agent-conversation-test',
    'input_message_ids', json_array('invalid-missing-input-message'),
    'created_at', '2026-09-15T00:00:06Z'
  )
),
(
  'invalid-source-cross-agent', 7, 'other-agent',
  '2026-09-15T00:00:07Z',
  json_object(
    'turn_id', 'invalid-source-cross-agent',
    'turn_index', 7,
    'agent_id', 'other-agent',
    'input_message_ids', json_array('invalid-cross-agent-message'),
    'created_at', '2026-09-15T00:00:07Z'
  )
),
(
  'invalid-source-missing-input', 8, 'agent-conversation-test',
  '2026-09-15T00:00:08Z',
  json_object(
    'turn_id', 'invalid-source-missing-input',
    'turn_index', 8,
    'agent_id', 'agent-conversation-test',
    'input_message_ids', json_array(),
    'created_at', '2026-09-15T00:00:08Z'
  )
);

INSERT INTO conversation_input_assignments (
  message_id, agent_id, turn_id, revision, assigned_at
) VALUES
(
  'invalid-incomplete-message', 'agent-conversation-test',
  'invalid-replay-incomplete', 7, '2026-09-15T00:00:01Z'
),
(
  'invalid-missing-input-message', 'agent-conversation-test',
  'invalid-replay-missing-input', 7, '2026-09-15T00:00:02Z'
),
(
  'invalid-missing-source-message', 'agent-conversation-test',
  'invalid-replay-missing-source', 7, '2026-09-15T00:00:03Z'
),
(
  'invalid-cross-agent-message', 'agent-conversation-test',
  'invalid-replay-cross-agent', 7, '2026-09-15T00:00:04Z'
),
(
  'invalid-source-missing-input-message', 'agent-conversation-test',
  'invalid-replay-source-missing-input', 7, '2026-09-15T00:00:05Z'
);

DELETE FROM schema_migrations WHERE version = 66;
"#,
    )?;
    drop(db);

    let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
    assert_eq!(db.current_schema_version()?, 66);
    let cleaned_replay_count: i64 = db.connection()?.query_row(
        "SELECT COUNT(*)
         FROM turn_records
         WHERE turn_id LIKE 'invalid-replay-%'
           AND json_type(payload_json, '$.replay') IS NULL",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(cleaned_replay_count, 5);
    let preserved_assignment_count: i64 = db.connection()?.query_row(
        "SELECT COUNT(*)
         FROM conversation_input_assignments
         WHERE turn_id LIKE 'invalid-replay-%'
           AND agent_id = 'agent-conversation-test'
           AND revision = 7",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(preserved_assignment_count, 5);
    Ok(())
}

#[test]
fn terminal_result_attention_matrix_is_typed() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    let cases = [
        (TurnTerminalKind::Completed, None),
        (TurnTerminalKind::Aborted, Some(Attention::Interrupted)),
        (
            TurnTerminalKind::BaselineOverBudget,
            Some(Attention::Failed {
                outcome: TerminalOutcome::BaselineOverBudget,
            }),
        ),
        (
            TurnTerminalKind::DeferredToFallback,
            Some(Attention::Interrupted),
        ),
        (
            TurnTerminalKind::ProviderFailedNeedsRecovery,
            Some(Attention::Interrupted),
        ),
    ];
    for (index, (kind, _)) in cases.iter().enumerate() {
        db.turn_records().upsert(&terminal(
            turn(&format!("turn-{index}"), index as u64 + 1),
            *kind,
            None,
        ))?;
    }
    db.turn_records().upsert(&terminal(
        turn("turn-no-brief", 10),
        TurnTerminalKind::Completed,
        Some(TurnNoBriefReason::ToolOnlyWait),
    ))?;

    let page = db.conversation().summary_page(AGENT_ID, 10, None, None)?;
    for (index, (kind, expected_attention)) in cases.iter().enumerate() {
        let summary = page
            .turns
            .iter()
            .find(|turn| turn.turn_id == format!("turn-{index}"))
            .expect("matrix turn");
        assert_eq!(
            summary.execution,
            ExecutionState::Terminal {
                outcome: (*kind).into(),
            }
        );
        assert_eq!(&summary.attention, expected_attention);
        assert_eq!(summary.result, ResultState::Pending);
        assert!(!summary.settled);
    }
    let no_brief = page
        .turns
        .iter()
        .find(|turn| turn.turn_id == "turn-no-brief")
        .expect("no-brief turn");
    assert_eq!(
        no_brief.result,
        ResultState::None {
            reason: NoBriefReason::ToolOnlyWait,
        }
    );
    assert!(no_brief.settled);
    Ok(())
}

#[test]
fn settled_result_without_canonical_linkage_is_unavailable() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    db.turn_records().upsert(&terminal(
        turn("turn-missing-canonical-result", 1),
        TurnTerminalKind::Completed,
        None,
    ))?;
    db.transaction(|tx| {
        settle_turn_result_tx(tx, AGENT_ID, "turn-missing-canonical-result", timestamp(2))
            .map(|_| ())
    })?;

    let page = db.conversation().summary_page(AGENT_ID, 1, None, None)?;
    assert_eq!(
        page.turns[0].result,
        ResultState::Unavailable {
            reason: crate::domain::conversation::ResultUnavailableReason::MissingCanonicalLinkage,
            retryable: false,
        }
    );
    assert!(page.turns[0].settled);
    Ok(())
}

#[test]
fn turn_and_brief_revisions_are_idempotent_under_late_brief_race() -> Result<()> {
    let (_temp_dir, db_path, lock_path, db) = runtime_db()?;
    let active = turn("turn-late-brief", 1);
    db.turn_records().upsert(&active)?;
    db.turn_records().upsert(&active)?;
    let created = db.conversation().summary_page(AGENT_ID, 1, None, None)?;
    assert_eq!(created.turns[0].revision, 1);

    let completed = terminal(active, TurnTerminalKind::Completed, None);
    db.turn_records().upsert(&completed)?;
    let terminal_page = db.conversation().summary_page(AGENT_ID, 1, None, None)?;
    assert_eq!(terminal_page.turns[0].revision, 2);
    assert_eq!(terminal_page.turns[0].result, ResultState::Pending);
    assert!(!terminal_page.turns[0].settled);

    let mut brief = BriefRecord::new(AGENT_ID, BriefKind::Result, "late result", None, None);
    brief.id = "brief-late".into();
    brief.turn_id = Some("turn-late-brief".into());
    brief.turn_index = Some(1);
    brief.created_at = timestamp(20);

    let barrier = Arc::new(Barrier::new(2));
    let mut joins = Vec::new();
    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        let db_path = db_path.clone();
        let lock_path = lock_path.clone();
        let brief = brief.clone();
        joins.push(std::thread::spawn(move || -> Result<()> {
            let db = RuntimeDb::open_and_migrate(db_path, lock_path)?;
            barrier.wait();
            db.evidence().append_brief(&brief)
        }));
    }
    for join in joins {
        join.join().expect("late brief writer")?;
    }

    let available = db.conversation().summary_page(AGENT_ID, 1, None, None)?;
    assert_eq!(available.turns[0].revision, 3);
    assert_eq!(available.turns[0].brief_ids, ["brief-late"]);
    assert_eq!(available.turns[0].result, ResultState::Available);
    assert!(!available.turns[0].settled);

    db.transaction(|tx| {
        settle_turn_result_tx(tx, AGENT_ID, "turn-late-brief", timestamp(30)).map(|_| ())
    })?;
    db.transaction(|tx| {
        settle_turn_result_tx(tx, AGENT_ID, "turn-late-brief", timestamp(31)).map(|_| ())
    })?;
    let settled = db.conversation().summary_page(AGENT_ID, 1, None, None)?;
    assert_eq!(settled.turns[0].revision, 4);
    assert_eq!(settled.turns[0].result, ResultState::Available);
    assert!(settled.turns[0].settled);
    Ok(())
}

#[test]
fn pending_input_tracks_queue_assignment_without_disappearing() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    let mut pending_message = MessageEnvelope::new(
        AGENT_ID,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator-test".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "check the pending echo".into(),
        },
    );
    pending_message.id = "message-pending".into();
    pending_message.created_at = timestamp(1);
    db.evidence().append_message(&pending_message)?;
    let queued = QueueEntryRecord {
        message_id: "message-pending".into(),
        agent_id: AGENT_ID.into(),
        priority: Priority::Normal,
        status: QueueEntryStatus::Queued,
        created_at: timestamp(1),
        updated_at: timestamp(1),
    };
    db.queue_entries().upsert(&queued)?;
    let page = db.conversation().summary_page(AGENT_ID, 10, None, None)?;
    assert_eq!(page.pending_inputs.len(), 1);
    assert_eq!(page.pending_inputs[0].revision, 2);
    assert_eq!(page.pending_inputs[0].state, PendingInputState::Queued);
    assert!(
        page.pending_inputs[0]
            .preview
            .contains("check the pending echo"),
        "pending preview should carry the input text: {}",
        page.pending_inputs[0].preview
    );

    let assigning = QueueEntryRecord {
        status: QueueEntryStatus::Dequeued,
        updated_at: timestamp(2),
        ..queued
    };
    db.queue_entries().upsert(&assigning)?;
    let page = db.conversation().summary_page(AGENT_ID, 10, None, None)?;
    assert_eq!(page.pending_inputs[0].revision, 3);
    assert_eq!(page.pending_inputs[0].state, PendingInputState::Assigning);
    assert!(page.pending_inputs[0]
        .preview
        .contains("check the pending echo"));

    let mut assigned = turn("turn-assigned", 1);
    assigned.input_message_ids = vec!["message-pending".into()];
    db.turn_records().upsert(&assigned)?;
    let page = db.conversation().summary_page(AGENT_ID, 10, None, None)?;
    assert!(page.pending_inputs.is_empty());
    assert_eq!(page.active_turns[0].turn_id, "turn-assigned");
    Ok(())
}

#[test]
fn transcript_coverage_changes_advance_the_turn_summary_revision() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    db.turn_records().upsert(&turn("turn-live-coverage", 1))?;
    let before = db
        .conversation()
        .activities(AGENT_ID, "turn-live-coverage", 10, None, None)?
        .expect("active turn");
    let entry = TranscriptEntry::new(
        AGENT_ID,
        TranscriptEntryKind::ToolResults,
        Some(1),
        None,
        serde_json::json!({"turn_id": "turn-live-coverage", "results": []}),
    );
    db.evidence().append_transcript_entry(&entry)?;
    let after = db
        .conversation()
        .activities(AGENT_ID, "turn-live-coverage", 10, None, None)?
        .expect("updated turn");
    assert_ne!(before.turn.detail_coverage, after.turn.detail_coverage);
    assert!(after.turn.revision > before.turn.revision);
    db.evidence().append_transcript_entry(&entry)?;
    let replay = db
        .conversation()
        .activities(AGENT_ID, "turn-live-coverage", 10, None, None)?
        .expect("replayed turn");
    assert_eq!(after.turn, replay.turn);
    Ok(())
}

#[test]
fn activity_keyset_is_stable_and_source_updates_replace_in_place() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    let mut message = MessageEnvelope::new(
        AGENT_ID,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "operator input".into(),
        },
    );
    message.id = "message-activity".into();
    message.created_at = timestamp(1);
    db.evidence().append_message(&message)?;

    let mut record = turn("turn-activity", 1);
    record.input_message_ids = vec![message.id.clone()];
    record.tool_execution_ids = vec!["tool-activity".into()];
    db.turn_records().upsert(&record)?;

    let mut assistant = TranscriptEntry::new(
        AGENT_ID,
        TranscriptEntryKind::AssistantRound,
        Some(1),
        Some(message.id.clone()),
        serde_json::json!({
            "turn_id": "turn-activity",
            "text": "assistant activity",
        }),
    );
    assistant.id = "assistant-activity".into();
    assistant.created_at = timestamp(2);
    db.evidence().append_transcript_entry(&assistant)?;

    let mut tool = ToolExecutionRecord {
        id: "tool-activity".into(),
        agent_id: AGENT_ID.into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: Some("turn-activity".into()),
        tool_name: "ExecCommand".into(),
        created_at: timestamp(3),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Deferred,
        input: serde_json::json!({ "cmd": "true" }),
        output: serde_json::Value::Null,
        summary: "tool pending".into(),
        invocation_surface: None,
    };
    db.evidence().append_tool_execution(&tool)?;

    let first = db
        .conversation()
        .activities(AGENT_ID, "turn-activity", 2, None, None)?
        .expect("activity turn");
    assert_eq!(
        first
            .activities
            .iter()
            .map(|activity| activity_item(activity).id.as_str())
            .collect::<Vec<_>>(),
        ["assistant:assistant-activity", "tool:tool-activity"]
    );
    assert_eq!(
        activity_item(&first.activities[1]).summary,
        "ExecCommand · deferred"
    );
    assert!(first.has_more);
    let before = first.next_before.clone().expect("older activity cursor");
    let upper_bound = first
        .membership_upper_bound
        .clone()
        .expect("activity upper bound");

    tool.status = ToolExecutionStatus::Success;
    tool.completed_at = Some(timestamp(4));
    tool.duration_ms = 1;
    tool.summary = "tool complete".into();
    db.evidence().append_tool_execution(&tool)?;
    let updated = db
        .conversation()
        .activities(AGENT_ID, "turn-activity", 10, None, None)?
        .expect("updated activity turn");
    let updated_tool = updated
        .activities
        .iter()
        .find(|activity| activity_item(activity).id == "tool:tool-activity")
        .expect("updated tool");
    assert_eq!(activity_item(updated_tool).summary, "ExecCommand · success");
    assert_eq!(activity_item(updated_tool).revision, 2);
    assert_eq!(
        activity_item(updated_tool).key.event_seq,
        upper_bound.event_seq
    );
    assert!(updated.detail_revision > first.detail_revision);

    let mut error = TranscriptEntry::new(
        AGENT_ID,
        TranscriptEntryKind::RuntimeFailure,
        Some(2),
        None,
        serde_json::json!({
            "turn_id": "turn-activity",
            "error": "late error",
        }),
    );
    error.id = "error-late".into();
    error.created_at = timestamp(5);
    db.evidence().append_transcript_entry(&error)?;

    let older = db
        .conversation()
        .activities(
            AGENT_ID,
            "turn-activity",
            2,
            Some(&before),
            Some(&upper_bound),
        )?
        .expect("older activities");
    assert_eq!(
        older
            .activities
            .iter()
            .map(|activity| activity_item(activity).id.as_str())
            .collect::<Vec<_>>(),
        ["operator:message-activity"]
    );
    assert!(!older.has_more);
    Ok(())
}

#[test]
fn keyset_queries_use_declared_indexes_without_temp_sorting() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    let connection = db.connection()?;

    let history_plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {CONVERSATION_HISTORY_BEFORE_SQL}"
        ))?
        .query_map(params![AGENT_ID, 10_i64, "z", 5_i64, "m", 10_i64], |row| {
            row.get::<_, String>(3)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(
        history_plan
            .iter()
            .any(|detail| detail.contains("idx_turn_records_agent_keyset")),
        "history plan did not use keyset index: {history_plan:?}"
    );
    assert!(
        history_plan
            .iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE")),
        "history plan used a temporary sort: {history_plan:?}"
    );

    let activity_plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {CONVERSATION_ACTIVITY_BEFORE_SQL}"
        ))?
        .query_map(
            params![AGENT_ID, "turn", 10_i64, 5_i64, "tool:z", 10_i64],
            |row| row.get::<_, String>(3),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(
        activity_plan
            .iter()
            .any(|detail| detail.contains("idx_conversation_sources_agent_turn_activity")),
        "activity plan did not use keyset index: {activity_plan:?}"
    );
    assert!(
        activity_plan
            .iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE")),
        "activity plan used a temporary sort: {activity_plan:?}"
    );
    Ok(())
}

#[test]
fn change_batch_recovers_snapshot_gap_and_coalesces_completed_turn() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    register_public_agent(&db)?;
    db.turn_records().upsert(&terminal(
        turn("turn-stream-completed", 1),
        TurnTerminalKind::Completed,
        None,
    ))?;
    let snapshot = db
        .conversation()
        .summary_snapshot(AGENT_ID, 10, None, "test-principal", "public")?
        .expect("conversation snapshot");
    let snapshot_revision = snapshot.value.turns[0].revision;

    let mut brief = BriefRecord::new(AGENT_ID, BriefKind::Result, "result", None, None);
    brief.id = "brief-stream-completed".into();
    brief.turn_id = Some("turn-stream-completed".into());
    brief.turn_index = Some(1);
    brief.created_at = timestamp(10);
    let event = crate::types::brief_created_event_for(&brief)?;
    db.evidence()
        .append_brief_with_created_event(Some(AGENT_ID), &brief, &event, &[])?;
    append_turn_event(
        &db,
        "event-stream-completed-extra",
        "turn-stream-completed",
        11,
    )?;

    let batch = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&snapshot.snapshot_cursor),
            10,
            10,
            "test-principal",
            "public",
        )?
        .expect("change batch");
    assert_eq!(batch.from_seq, snapshot.event_head_seq);
    assert!(batch.through_seq > batch.from_seq);
    let summaries = batch
        .changes
        .iter()
        .filter_map(|change| match change {
            ConversationChange::TurnSummaryUpsert { turn } => Some(turn),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].turn_id, "turn-stream-completed");
    assert_eq!(summaries[0].brief_ids, ["brief-stream-completed"]);
    assert!(summaries[0].revision > snapshot_revision);
    assert_eq!(
        batch
            .changes
            .iter()
            .filter(|change| matches!(
                change,
                ConversationChange::DetailInvalidated { turn_id, .. }
                    if turn_id == "turn-stream-completed"
            ))
            .count(),
        1
    );

    let reconnected = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&batch.checkpoint),
            10,
            10,
            "test-principal",
            "public",
        )?
        .expect("reconnected batch");
    assert_eq!(reconnected.from_seq, batch.through_seq);
    assert_eq!(reconnected.through_seq, batch.through_seq);
    assert!(reconnected.changes.is_empty());
    Ok(())
}

#[test]
fn change_id_collection_ignores_values_beyond_the_depth_budget() {
    let mut nested = serde_json::json!({
        "turn_id": "turn-too-deep",
        "message_id": "message-too-deep",
    });
    for _ in 0..=MAX_CHANGE_ID_JSON_DEPTH {
        nested = serde_json::json!({ "nested": nested });
    }
    let payload = serde_json::json!({
        "turn_id": "turn-visible",
        "message_id": "message-visible",
        "nested": nested,
    });
    let mut turn_ids = std::collections::BTreeSet::new();
    let mut message_ids = std::collections::BTreeSet::new();

    collect_change_ids(&payload, &mut turn_ids, &mut message_ids, 0);

    assert_eq!(turn_ids.into_iter().collect::<Vec<_>>(), ["turn-visible"]);
    assert_eq!(
        message_ids.into_iter().collect::<Vec<_>>(),
        ["message-visible"]
    );
}

#[test]
fn change_batch_reconciles_brief_before_terminal_and_bounds_active_activity() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    register_public_agent(&db)?;
    let mut active = turn("turn-stream-active", 1);
    db.turn_records().upsert(&active)?;
    let snapshot = db
        .conversation()
        .summary_snapshot(AGENT_ID, 10, None, "test-principal", "public")?
        .expect("conversation snapshot");
    let snapshot_detail_revision = db
        .conversation()
        .activities(AGENT_ID, "turn-stream-active", 1, None, None)?
        .expect("active turn detail")
        .detail_revision;

    let mut brief = BriefRecord::new(AGENT_ID, BriefKind::Result, "early result", None, None);
    brief.id = "brief-stream-active".into();
    brief.turn_id = Some("turn-stream-active".into());
    brief.turn_index = Some(1);
    brief.created_at = timestamp(10);
    let event = crate::types::brief_created_event_for(&brief)?;
    db.evidence()
        .append_brief_with_created_event(Some(AGENT_ID), &brief, &event, &[])?;

    active.tool_execution_ids = vec!["tool-stream-a".into(), "tool-stream-b".into()];
    db.turn_records().upsert(&active)?;
    for (id, offset) in [("tool-stream-a", 11), ("tool-stream-b", 12)] {
        db.evidence().append_tool_execution(&ToolExecutionRecord {
            id: id.into(),
            agent_id: AGENT_ID.into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: Some("turn-stream-active".into()),
            tool_name: "ExecCommand".into(),
            created_at: timestamp(offset),
            completed_at: Some(timestamp(offset + 1)),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({ "cmd": "true" }),
            output: serde_json::Value::Null,
            summary: id.into(),
            invocation_surface: None,
        })?;
    }
    append_turn_event(&db, "event-stream-active-tools", "turn-stream-active", 13)?;

    let bounded_active_batch = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&snapshot.snapshot_cursor),
            10,
            1,
            "test-principal",
            "public",
        )?
        .expect("bounded active change batch");
    assert!(bounded_active_batch.changes.iter().any(|change| matches!(
        change,
        ConversationChange::DetailInvalidated { turn_id, .. }
            if turn_id == "turn-stream-active"
    )));
    assert!(!bounded_active_batch
        .changes
        .iter()
        .any(|change| matches!(change, ConversationChange::ActivityUpsert { .. })));

    let active_batch = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&snapshot.snapshot_cursor),
            10,
            2,
            "test-principal",
            "public",
        )?
        .expect("active change batch");
    let active_summary = active_batch
        .changes
        .iter()
        .find_map(|change| match change {
            ConversationChange::TurnSummaryUpsert { turn } => Some(turn),
            _ => None,
        })
        .expect("active summary upsert");
    assert!(matches!(active_summary.execution, ExecutionState::Active));
    assert_eq!(active_summary.brief_ids, ["brief-stream-active"]);
    assert_eq!(
        active_batch
            .changes
            .iter()
            .filter(|change| matches!(change, ConversationChange::ActivityUpsert { .. }))
            .count(),
        2
    );
    let invalidated_revision = active_batch
        .changes
        .iter()
        .find_map(|change| match change {
            ConversationChange::DetailInvalidated {
                detail_revision, ..
            } => Some(*detail_revision),
            _ => None,
        })
        .expect("detail invalidation");
    assert!(invalidated_revision > snapshot_detail_revision);
    let active_revision = active_summary.revision;

    db.turn_records()
        .upsert(&terminal(active, TurnTerminalKind::Completed, None))?;
    append_turn_event(
        &db,
        "event-stream-active-terminal",
        "turn-stream-active",
        14,
    )?;
    let terminal_batch = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&active_batch.checkpoint),
            10,
            1,
            "test-principal",
            "public",
        )?
        .expect("terminal change batch");
    let terminal_summary = terminal_batch
        .changes
        .iter()
        .find_map(|change| match change {
            ConversationChange::TurnSummaryUpsert { turn } => Some(turn),
            _ => None,
        })
        .expect("terminal summary upsert");
    assert!(matches!(
        terminal_summary.execution,
        ExecutionState::Terminal {
            outcome: TerminalOutcome::Completed
        }
    ));
    assert_eq!(terminal_summary.brief_ids, ["brief-stream-active"]);
    assert!(terminal_summary.revision > active_revision);
    Ok(())
}

#[test]
fn change_batch_enforces_shared_activity_budget_across_active_turns() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    register_public_agent(&db)?;
    for (turn_id, turn_index, tool_id, offset) in [
        ("turn-stream-budget-a", 1, "tool-stream-budget-a", 1),
        ("turn-stream-budget-b", 2, "tool-stream-budget-b", 2),
    ] {
        let mut active = turn(turn_id, turn_index);
        active.tool_execution_ids = vec![tool_id.into()];
        db.turn_records().upsert(&active)?;
        db.evidence().append_tool_execution(&ToolExecutionRecord {
            id: tool_id.into(),
            agent_id: AGENT_ID.into(),
            work_item_id: None,
            turn_index,
            turn_id: Some(turn_id.into()),
            tool_name: "ExecCommand".into(),
            created_at: timestamp(offset),
            completed_at: Some(timestamp(offset + 1)),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({ "cmd": "true" }),
            output: serde_json::Value::Null,
            summary: tool_id.into(),
            invocation_surface: None,
        })?;
    }
    let snapshot = db
        .conversation()
        .summary_snapshot(AGENT_ID, 10, None, "test-principal", "public")?
        .expect("conversation snapshot");
    append_turn_event(
        &db,
        "event-stream-shared-activity-budget",
        "turn-stream-budget-a",
        3,
    )?;

    let zero_limit = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&snapshot.snapshot_cursor),
            10,
            0,
            "test-principal",
            "public",
        )
        .expect_err("zero activity limit must be rejected");
    assert!(matches!(
        zero_limit.downcast_ref::<ConversationReadError>(),
        Some(ConversationReadError::InvalidLimit {
            resource: "conversation change activities",
            actual: 0,
            ..
        })
    ));

    let shared_batch = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&snapshot.snapshot_cursor),
            10,
            1,
            "test-principal",
            "public",
        )?
        .expect("shared activity batch");
    assert_eq!(
        shared_batch
            .changes
            .iter()
            .filter(|change| matches!(change, ConversationChange::ActivityUpsert { .. }))
            .count(),
        1
    );
    assert_eq!(
        shared_batch
            .changes
            .iter()
            .filter(|change| matches!(change, ConversationChange::DetailInvalidated { .. }))
            .count(),
        2
    );
    Ok(())
}

#[test]
fn change_batch_keeps_progress_when_active_detail_exceeds_inline_limit() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    register_public_agent(&db)?;
    let mut active = turn("turn-stream-large-active", 1);
    db.turn_records().upsert(&active)?;
    let snapshot = db
        .conversation()
        .summary_snapshot(AGENT_ID, 10, None, "test-principal", "public")?
        .expect("conversation snapshot");

    active.tool_execution_ids = (0..=MAX_CONVERSATION_CHANGE_ACTIVITIES)
        .map(|index| format!("tool-stream-large-{index:03}"))
        .collect();
    db.turn_records().upsert(&active)?;
    for (index, tool_id) in active.tool_execution_ids.iter().enumerate() {
        db.evidence().append_tool_execution(&ToolExecutionRecord {
            id: tool_id.clone(),
            agent_id: AGENT_ID.into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: Some(active.turn_id.clone()),
            tool_name: "ExecCommand".into(),
            created_at: timestamp(index as i64 + 1),
            completed_at: Some(timestamp(index as i64 + 2)),
            duration_ms: 1,
            authority_class: AuthorityClass::RuntimeInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({ "cmd": "true" }),
            output: serde_json::Value::Null,
            summary: tool_id.clone(),
            invocation_surface: None,
        })?;
    }
    append_turn_event(
        &db,
        "event-stream-large-active-first",
        "turn-stream-large-active",
        100,
    )?;

    let first = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&snapshot.snapshot_cursor),
            10,
            MAX_CONVERSATION_CHANGE_ACTIVITIES,
            "test-principal",
            "public",
        )?
        .expect("first large active batch");
    assert!(first.through_seq > snapshot.event_head_seq);
    assert!(first.changes.iter().any(|change| matches!(
        change,
        ConversationChange::TurnSummaryUpsert { turn }
            if turn.turn_id == "turn-stream-large-active"
    )));
    assert!(first.changes.iter().any(|change| matches!(
        change,
        ConversationChange::DetailInvalidated { turn_id, .. }
            if turn_id == "turn-stream-large-active"
    )));
    assert!(!first
        .changes
        .iter()
        .any(|change| matches!(change, ConversationChange::ActivityUpsert { .. })));

    append_turn_event(
        &db,
        "event-stream-large-active-second",
        "turn-stream-large-active",
        101,
    )?;
    let second = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&first.checkpoint),
            10,
            MAX_CONVERSATION_CHANGE_ACTIVITIES,
            "test-principal",
            "public",
        )?
        .expect("second large active batch");
    assert!(second.through_seq > first.through_seq);

    let detail = db
        .conversation()
        .activities(
            AGENT_ID,
            "turn-stream-large-active",
            MAX_CONVERSATION_CHANGE_ACTIVITIES,
            None,
            None,
        )?
        .expect("large active detail");
    assert_eq!(detail.activities.len(), MAX_CONVERSATION_CHANGE_ACTIVITIES);
    assert!(detail.has_more);
    assert!(detail.next_before.is_some());
    assert!(detail.membership_upper_bound.is_some());
    Ok(())
}

#[test]
fn change_batch_returns_typed_replay_retention_epoch_and_query_resets() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    register_public_agent(&db)?;
    let snapshot = db
        .conversation()
        .summary_snapshot(AGENT_ID, 10, None, "test-principal", "public")?
        .expect("conversation snapshot");
    let first_seq = append_turn_event(&db, "event-reset-first", "turn-reset", 1)?;
    let second_seq = append_turn_event(&db, "event-reset-second", "turn-reset", 2)?;

    let replay_error = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&snapshot.snapshot_cursor),
            1,
            10,
            "test-principal",
            "public",
        )
        .expect_err("bounded replay must reset");
    assert!(matches!(
        replay_error.downcast_ref::<ConversationReadError>(),
        Some(ConversationReadError::ResetRequired {
            reason: ConversationResetReason::ReplayLimitExceeded,
            ..
        })
    ));

    let signing_key: String = db.connection()?.query_row(
        "SELECT value FROM runtime_metadata
         WHERE key = 'conversation_cursor_signing_key'",
        [],
        |row| row.get(0),
    )?;
    let binding = CursorBinding {
        runtime_id: snapshot.runtime_id.clone(),
        agent_id: AGENT_ID.into(),
        event_log_epoch: snapshot.event_log_epoch.clone(),
        visibility_scope_id: snapshot.visibility_scope_id.clone(),
        schema_version: CONVERSATION_SCHEMA_VERSION,
        query_version: CONVERSATION_QUERY_VERSION,
    };
    let cursor = |binding, event_seq| {
        CursorCodec::new(signing_key.as_bytes()).encode(&StreamCursor { binding, event_seq })
    };

    let ahead = cursor(binding.clone(), second_seq + 1);
    let ahead_error = db
        .conversation()
        .change_batch(AGENT_ID, Some(&ahead), 10, 10, "test-principal", "public")
        .expect_err("cursor ahead must reset");
    assert!(matches!(
        ahead_error.downcast_ref::<ConversationReadError>(),
        Some(ConversationReadError::ResetRequired {
            reason: ConversationResetReason::CursorAhead,
            ..
        })
    ));

    let wrong_epoch = cursor(
        CursorBinding {
            event_log_epoch: "epoch_replaced".into(),
            ..binding.clone()
        },
        first_seq,
    );
    let epoch_error = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&wrong_epoch),
            10,
            10,
            "test-principal",
            "public",
        )
        .expect_err("epoch mismatch must reset");
    assert!(matches!(
        epoch_error.downcast_ref::<ConversationReadError>(),
        Some(ConversationReadError::Cursor(
            crate::domain::conversation::CursorDecodeError::EventLogEpochMismatch
        ))
    ));

    let wrong_query = cursor(
        CursorBinding {
            query_version: CONVERSATION_QUERY_VERSION + 1,
            ..binding
        },
        first_seq,
    );
    let query_error = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&wrong_query),
            10,
            10,
            "test-principal",
            "public",
        )
        .expect_err("query mismatch must reset");
    assert!(matches!(
        query_error.downcast_ref::<ConversationReadError>(),
        Some(ConversationReadError::Cursor(
            crate::domain::conversation::CursorDecodeError::QueryVersionMismatch { .. }
        ))
    ));

    db.connection()?.execute(
        "INSERT INTO audit_event_retention_watermarks (scope_key, oldest_retained_seq)
         VALUES (?1, ?2)",
        params![
            crate::runtime_db::evidence::audit_event_sequence_scope(Some(AGENT_ID)),
            i64::try_from(second_seq)?
        ],
    )?;
    let retention_error = db
        .conversation()
        .change_batch(
            AGENT_ID,
            Some(&snapshot.snapshot_cursor),
            10,
            10,
            "test-principal",
            "public",
        )
        .expect_err("expired cursor must reset");
    assert!(matches!(
        retention_error.downcast_ref::<ConversationReadError>(),
        Some(ConversationReadError::ResetRequired {
            reason: ConversationResetReason::RetentionExpired,
            ..
        })
    ));
    Ok(())
}

#[test]
fn activity_display_extracts_text_before_truncation_and_omits_provider_state() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    db.turn_records().upsert(&turn("turn-display", 1))?;
    let text = "可读的执行进度\n".repeat(1000);
    let mut entry = TranscriptEntry::new(
        AGENT_ID,
        TranscriptEntryKind::AssistantRound,
        Some(1),
        None,
        serde_json::json!({
            "turn_id": "turn-display",
            "checkpoint": "private-checkpoint",
            "blocks": [
                { "type": "thinking", "text": "private-reasoning", "signature": "private-signature" },
                { "type": "tool_use", "name": "ExecCommand", "input": { "secret": "private-input" } },
                { "type": "text", "text": text }
            ]
        }),
    );
    entry.id = "display-text".into();
    db.evidence().append_transcript_entry(&entry)?;
    let first = db
        .conversation()
        .activities(AGENT_ID, "turn-display", 10, None, None)?
        .unwrap();
    let summary = &activity_item(&first.activities[0]).summary;
    assert_eq!(summary, &text.chars().take(4000).collect::<String>());
    assert!(!summary.contains("private-"));

    entry.data["blocks"] = serde_json::json!([{ "type": "thinking", "text": "private-reasoning" }]);
    db.evidence().append_transcript_entry(&entry)?;
    let updated = db
        .conversation()
        .activities(AGENT_ID, "turn-display", 10, None, None)?
        .unwrap();
    assert!(activity_item(&updated.activities[0]).summary.is_empty());
    assert_eq!(activity_item(&updated.activities[0]).revision, 2);
    Ok(())
}

#[test]
fn summary_timing_uses_canonical_turn_records_without_brief_delivery() -> Result<()> {
    let (_temp_dir, _db_path, _lock_path, db) = runtime_db()?;
    let record = turn("timed-turn", 1);
    db.turn_records().upsert(&record)?;
    let active = db.conversation().summary_page(AGENT_ID, 10, None, None)?;
    assert_eq!(active.turns[0].started_at, record.created_at);
    assert_eq!(active.turns[0].completed_at, None);
    assert_eq!(active.turns[0].duration_ms, None);

    let mut finished = terminal(record, TurnTerminalKind::Aborted, None);
    // The measured duration is authoritative even if wall time differs.
    finished.terminal.as_mut().unwrap().duration_ms = 830;
    db.turn_records().upsert(&finished)?;
    let page = db.conversation().summary_page(AGENT_ID, 10, None, None)?;
    assert_eq!(page.turns[0].started_at, finished.created_at);
    assert_eq!(
        page.turns[0].completed_at,
        Some(finished.created_at + Duration::seconds(1))
    );
    assert_eq!(page.turns[0].duration_ms, Some(830));
    assert!(page.turns[0].brief_ids.is_empty());
    assert!(page.turns[0].revision > active.turns[0].revision);
    Ok(())
}
