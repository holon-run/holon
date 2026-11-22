//! Turn-content deletion and input-withdrawal transaction tests against a
//! real migrated runtime database. Never opens production data.

use anyhow::Result;
use chrono::Utc;

use super::turn_content_deletion::{
    delete_turn_content, preview_turn_content_deletion, TurnContentDeletionConflict,
    TurnContentDeletionError,
};
use super::RuntimeDb;
use crate::types::Priority;

fn database() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let directory = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )?;
    Ok((directory, db))
}

const TIME: &str = "2026-11-22T00:00:00.000Z";

fn turn_payload(input: &str, terminal_reason: &str, turn_index: i64) -> String {
    let record = holon_domain::TurnRecord {
        idle_input_batch: Some(holon_domain::IdleInputBatchRecord {
            anchor_message_id: format!("m-{input}"),
            activation_id: Some(format!("act-{input}")),
            sender_agent_id: "operator".into(),
            cutoff: Utc::now(),
            max_items: 8,
            max_bytes: 65536,
            rendered_bytes: input.len(),
            items: vec![holon_domain::IdleInputBatchItem {
                message_id: format!("m-{input}"),
                source_ref: format!("message:m-{input}"),
                rendered: format!("rendered body {input}"),
            }],
        }),
        terminal: Some(holon_domain::TurnTerminalSummary {
            kind: holon_domain::TurnTerminalKind::Completed,
            reason: Some(terminal_reason.to_owned()),
            no_brief_reason: None,
            completed_at: Utc::now(),
            duration_ms: 10,
        }),
        ..holon_domain::TurnRecord::new(
            "agent-a",
            format!("turn-{input}"),
            u64::try_from(turn_index).unwrap(),
        )
    };
    serde_json::to_string(&record).unwrap()
}

fn seed_turn(db: &RuntimeDb, turn_id: &str, turn_index: i64) -> Result<()> {
    let payload = turn_payload(&turn_id.replace("turn-", ""), "reducer_only/fixture", turn_index);
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO turn_records
               (turn_id, turn_index, agent_id, terminal_kind, created_at, completed_at, payload_json)
             VALUES (?1, ?2, 'agent-a', 'completed', ?3, ?3, ?4)",
            rusqlite::params![turn_id, turn_index, TIME, payload],
        )?;
        tx.execute(
            "INSERT INTO conversation_turn_revisions
               (agent_id, turn_id, summary_revision, detail_revision, result_settled, updated_at)
             VALUES ('agent-a', ?1, 1, 1, 1, ?2)",
            rusqlite::params![turn_id, TIME],
        )?;
        let message_id = format!("m-{turn_id}");
        tx.execute(
            "INSERT INTO messages
               (evidence_id, agent_id, turn_id, message_id, created_at, kind, preview, payload_json)
             VALUES (?1, 'agent-a', ?2, ?1, ?3, 'operator_prompt', 'preview-body', '{}')",
            rusqlite::params![message_id, turn_id, TIME],
        )?;
        tx.execute(
            "INSERT INTO briefs
               (evidence_id, agent_id, turn_id, created_at, kind, payload_json)
             VALUES (?1, 'agent-a', ?2, ?3, 'text', '{}')",
            rusqlite::params![format!("b-{turn_id}"), turn_id, TIME],
        )?;
        tx.execute(
            "INSERT INTO tool_executions
               (evidence_id, agent_id, turn_id, created_at, kind, payload_json)
             VALUES (?1, 'agent-a', ?2, ?3, 'tool', ?4)",
            rusqlite::params![
                format!("t-{turn_id}"),
                turn_id,
                TIME,
                serde_json::json!({
                    "id": format!("t-{turn_id}"),
                    "tool_name": "exec_command",
                    "input": {"cmd": "echo secret", "path": "tool-artifacts/x.log"}
                })
                .to_string()
            ],
        )?;
        tx.execute(
            "INSERT INTO conversation_source_revisions
               (source_kind, source_id, agent_id, turn_id, activity_seq, revision, updated_at)
             VALUES ('operator', ?1, 'agent-a', ?2, ?3, 1, ?4)",
            rusqlite::params![message_id, turn_id, turn_index * 10, TIME],
        )?;
        tx.execute(
            "INSERT INTO conversation_input_assignments
               (message_id, agent_id, turn_id, revision, assigned_at)
             VALUES (?1, 'agent-a', ?2, 1, ?3)",
            rusqlite::params![message_id, turn_id, TIME],
        )?;
        Ok(())
    })
}

fn seed_agent_state(db: &RuntimeDb) -> Result<()> {
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO agent_states (agent_id, status, updated_at, payload_json)
             VALUES ('agent-a', 'asleep', ?1, ?2)",
            rusqlite::params![
                TIME,
                serde_json::json!({
                    "id": "agent-a",
                    "status": "asleep",
                    "working_memory": {
                        "compression_epoch": 4,
                        "active_episode_builder": {
                            "id": "ep-builder-1",
                            "started_at": TIME,
                            "start_turn_index": 1,
                            "latest_turn_index": 3,
                            "source_turn_ids": ["turn-b"]
                        }
                    }
                })
                .to_string()
            ],
        )?;
        Ok(())
    })
}

fn turn_row(db: &RuntimeDb, turn_id: &str) -> (Option<String>, String) {
    db.with_read(|connection| {
        connection
            .query_row(
                "SELECT content_deleted_at, payload_json FROM turn_records WHERE turn_id = ?1",
                rusqlite::params![turn_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(anyhow::Error::from)
    })
    .unwrap()
}

fn count(db: &RuntimeDb, sql: &str, turn_id: &str) -> i64 {
    db.with_read(|connection| {
        connection
            .query_row(sql, rusqlite::params![turn_id], |row| row.get(0))
            .map_err(anyhow::Error::from)
    })
    .unwrap()
}

fn count_all(db: &RuntimeDb, sql: &str) -> i64 {
    db.with_read(|connection| {
        connection
            .query_row(sql, [], |row| row.get(0))
            .map_err(anyhow::Error::from)
    })
        .unwrap()
}

#[test]
fn delete_turn_removes_exclusive_content_and_is_idempotent() -> Result<()> {
    let (_dir, db) = database()?;
    seed_agent_state(&db)?;
    seed_turn(&db, "turn-a", 1)?;
    seed_turn(&db, "turn-b", 2)?;
    seed_turn(&db, "turn-c", 3)?;

    let preview = preview_turn_content_deletion(&db, "agent-a", "turn-b")?.unwrap();
    assert_eq!(preview.input_count, 1);
    assert_eq!(preview.brief_count, 1);
    assert_eq!(preview.tool_execution_count, 1);
    assert!(!preview.already_deleted);

    let now = Utc::now();
    let deletion = delete_turn_content(&db, "agent-a", "turn-b", Some(1), now)?
        .expect("deletion succeeds");
    assert!(!deletion.already_deleted);
    assert_eq!(deletion.removed_evidence, 3);

    // Exclusive rows are gone; neighbors untouched.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM messages WHERE turn_id = ?1", "turn-b"), 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM briefs WHERE turn_id = ?1", "turn-b"), 0);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM tool_executions WHERE turn_id = ?1", "turn-b"),
        0
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM conversation_source_revisions WHERE turn_id = ?1", "turn-b"),
        0
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM messages WHERE turn_id = ?1", "turn-a"), 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM messages WHERE turn_id = ?1", "turn-c"), 1);

    // Marker + erased payload keep identity, activation ids, and timing.
    let (deleted_at, payload) = turn_row(&db, "turn-b");
    let deleted_at = deleted_at.expect("marker set");
    let record: holon_domain::TurnRecord = serde_json::from_str(&payload).unwrap();
    assert!(record.idle_input_batch.as_ref().unwrap().items[0].rendered.is_empty());
    assert_eq!(
        record.idle_input_batch.as_ref().unwrap().activation_id.as_deref(),
        Some("act-b")
    );
    assert!(record.terminal.as_ref().unwrap().reason.is_none());
    assert_eq!(record.turn_index, 2);
    // The activation trigger keeps its derived edge for the surviving payload.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM turn_activation_attachments WHERE turn_id = ?1",
            "turn-b"
        ),
        1
    );

    // Retained-row scrubbing, audit trail, outbox, artifact queue.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM audit_events WHERE kind = 'turn_content_deleted' \
             AND json_extract(data_json, '$.data.turn_id') = ?1",
            "turn-b"
        ),
        1
    );
    assert!(count_all(
        &db,
        "SELECT COUNT(*) FROM runtime_index_outbox WHERE operation = 'delete' \
         AND reason = 'turn_content_deleted'",
    ) >= 3);
    assert_eq!(
        count_all(
            &db,
            "SELECT COUNT(*) FROM pending_artifact_deletions WHERE relative_path = 'tool-artifacts/x.log'",
        ),
        1
    );
    // Working memory: epoch bumped, builder that named turn-b reset.
    let (epoch, builder): (i64, Option<String>) = db
        .with_read(|connection| {
            connection
                .query_row(
                    "SELECT json_extract(payload_json, '$.working_memory.compression_epoch'), \
                            json_extract(payload_json, '$.working_memory.active_episode_builder') \
                     FROM agent_states WHERE agent_id = 'agent-a'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(anyhow::Error::from)
        })
        .unwrap();
    assert_eq!(epoch, 5);
    assert!(builder.is_none());

    // Idempotent retry keeps the first timestamp.
    let retry = delete_turn_content(&db, "agent-a", "turn-b", None, Utc::now())?
        .expect("retry succeeds");
    assert!(retry.already_deleted);
    assert_eq!(retry.content_deleted_at, deleted_at);
    Ok(())
}

#[test]
fn delete_turn_rejects_active_shared_and_sealed() -> Result<()> {
    let (_dir, db) = database()?;
    seed_agent_state(&db)?;
    seed_turn(&db, "turn-x", 1)?;

    // Active execution: terminal cleared.
    db.transaction(|tx| {
        tx.execute(
            "UPDATE turn_records SET terminal_kind = NULL, completed_at = NULL \
             WHERE turn_id = 'turn-x'",
            [],
        )?;
        Ok(())
    })?;
    let error = delete_turn_content(&db, "agent-a", "turn-x", None, Utc::now())?.unwrap_err();
    assert_eq!(
        error,
        TurnContentDeletionError::Conflict(TurnContentDeletionConflict::ActiveExecution)
    );
    db.transaction(|tx| {
        tx.execute(
            "UPDATE turn_records SET terminal_kind = 'completed', completed_at = ?1 \
             WHERE turn_id = 'turn-x'",
            rusqlite::params![TIME],
        )?;
        Ok(())
    })?;

    // Shared content: the activity spine attributes our message to another turn.
    seed_turn(&db, "turn-y", 2)?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE conversation_source_revisions \
             SET turn_id = 'turn-y', activity_seq = 999 \
             WHERE source_kind = 'operator' AND source_id = 'm-turn-x'",
            [],
        )?;
        Ok(())
    })?;
    let error = delete_turn_content(&db, "agent-a", "turn-x", None, Utc::now())?.unwrap_err();
    assert_eq!(
        error,
        TurnContentDeletionError::Conflict(TurnContentDeletionConflict::SharedContent)
    );

    // Sealed reference: mandatory eligibility names the turn.
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM conversation_source_revisions WHERE activity_seq = 999",
            [],
        )?;
        tx.execute(
            "INSERT INTO episode_continuity_seeds
               (seed_id, agent_id, policy_version, boundary_id, source_kind,
                roots_json, candidate_payloads_json, import_required, captured_at)
             VALUES ('seed-1', 'agent-a', 1, 'boundary-1', 'upgrade_identity',
                     '{}', '[]', 0, ?1)",
            rusqlite::params![TIME],
        )?;
        tx.execute(
            "INSERT INTO input_evidence_eligibility
               (seed_id, capture_phase, family, record_id, fingerprint_json, mandatory)
             VALUES ('seed-1', 'upgrade', 'turn', 'turn-x', '{}', 1)",
            [],
        )?;
        Ok(())
    })?;
    let error = delete_turn_content(&db, "agent-a", "turn-x", None, Utc::now())?.unwrap_err();
    assert_eq!(
        error,
        TurnContentDeletionError::Conflict(TurnContentDeletionConflict::SealedReference)
    );

    // Rejections deleted nothing.
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM messages WHERE turn_id = ?1", "turn-x"),
        1
    );
    assert!(turn_row(&db, "turn-x").0.is_none());

    // Revision mismatch rejects before mutation.
    let error =
        delete_turn_content(&db, "agent-a", "turn-y", Some(99), Utc::now())?.unwrap_err();
    assert_eq!(
        error,
        TurnContentDeletionError::Conflict(TurnContentDeletionConflict::RevisionConflict)
    );

    // Unknown turn: typed not-found, body-free.
    let error = delete_turn_content(&db, "agent-a", "turn-missing", None, Utc::now())?.unwrap_err();
    assert_eq!(error, TurnContentDeletionError::NotFound);
    Ok(())
}

#[test]
fn delete_turn_invalidates_covering_episodes() -> Result<()> {
    let (_dir, db) = database()?;
    seed_agent_state(&db)?;
    seed_turn(&db, "turn-b", 2)?;
    db.transaction(|tx| {
        // Interval-covering episode without source turn ids (hard-capped shape).
        tx.execute(
            "INSERT INTO context_episode_anchors
               (episode_id, agent_id, workspace_id, boundary_reason,
                start_turn_index, end_turn_index, started_at, ended_at, payload_json)
             VALUES ('ep-1', 'agent-a', 'agent_home:agent-a', 'work_switch',
                     1, 3, ?1, ?1, ?2)",
            rusqlite::params![
                TIME,
                serde_json::json!({
                    "id": "ep-1", "agent_id": "agent-a",
                    "workspace_id": "agent_home:agent-a",
                    "created_at": TIME, "finalized_at": TIME,
                    "start_turn_index": 1, "end_turn_index": 3,
                    "boundary_reason": "work_switch", "source_turn_ids": []
                })
                .to_string()
            ],
        )?;
        // Disjoint episode stays.
        tx.execute(
            "INSERT INTO context_episode_anchors
               (episode_id, agent_id, workspace_id, boundary_reason,
                start_turn_index, end_turn_index, started_at, ended_at, payload_json)
             VALUES ('ep-2', 'agent-a', 'agent_home:agent-a', 'work_switch',
                     5, 6, ?1, ?1, ?2)",
            rusqlite::params![
                TIME,
                serde_json::json!({
                    "id": "ep-2", "agent_id": "agent-a",
                    "workspace_id": "agent_home:agent-a",
                    "created_at": TIME, "finalized_at": TIME,
                    "start_turn_index": 5, "end_turn_index": 6,
                    "boundary_reason": "work_switch", "source_turn_ids": []
                })
                .to_string()
            ],
        )?;
        Ok(())
    })?;
    let deletion =
        delete_turn_content(&db, "agent-a", "turn-b", None, Utc::now())?.expect("deletes");
    assert_eq!(deletion.invalidated_episodes, 1);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM context_episode_anchors WHERE episode_id = ?1",
            "ep-1"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM context_episode_anchors WHERE episode_id = ?1",
            "ep-2"
        ),
        1
    );
    Ok(())
}

fn queue_entry(message_id: &str, status: &str, origin_kind: &str) -> String {
    serde_json::json!({
        "message_id": message_id,
        "agent_id": "agent-a",
        "priority": "normal",
        "status": status,
        "created_at": TIME,
        "updated_at": TIME
    })
    .to_string()
    .replace("__ORIGIN__", origin_kind)
}

fn seed_queue(db: &RuntimeDb, message_id: &str, status: &str, origin_kind: &str) -> Result<()> {
    db.transaction(|tx| {
        tx.execute(
            "INSERT INTO queue_entries
               (message_id, agent_id, priority, status, created_at, updated_at, payload_json)
             VALUES (?1, 'agent-a', 'normal', ?2, ?3, ?3, ?4)",
            rusqlite::params![message_id, status, TIME, queue_entry(message_id, status, origin_kind)],
        )?;
        tx.execute(
            "INSERT INTO messages
               (evidence_id, agent_id, turn_id, message_id, created_at, kind, payload_json)
             VALUES (?1, 'agent-a', NULL, ?1, ?2, 'operator_prompt', ?3)",
            rusqlite::params![
                message_id,
                TIME,
                serde_json::json!({"id": message_id, "origin": {"kind": origin_kind}}).to_string()
            ],
        )?;
        Ok(())
    })
}

#[test]
fn withdraw_input_cas_idempotency_and_conflicts() -> Result<()> {
    use super::input_withdrawal::{withdraw_input, InputWithdrawalError};

    let (_dir, db) = database()?;
    seed_queue(&db, "m-queued", "queued", "operator")?;
    seed_queue(&db, "m-claimed", "dequeued", "operator")?;
    seed_queue(&db, "m-system", "queued", "system")?;

    // Withdrawal wins the CAS from queued.
    let withdrawal = withdraw_input(&db, "agent-a", "m-queued", Utc::now())?.expect("withdraws");
    assert!(withdrawal.first_withdrawal);
    let status: String = db
        .with_read(|connection| {
            connection
                .query_row(
                    "SELECT status FROM queue_entries WHERE message_id = 'm-queued'",
                    [],
                    |row| row.get(0),
                )
                .map_err(anyhow::Error::from)
        })
        .unwrap();
    assert_eq!(status, "withdrawn");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM messages WHERE evidence_id = ?1",
            "m-queued"
        ),
        0
    );

    // Retry is idempotent even though the messages row is gone.
    let retry = withdraw_input(&db, "agent-a", "m-queued", Utc::now())?.expect("idempotent");
    assert!(!retry.first_withdrawal);
    assert_eq!(retry.withdrawn_at, withdrawal.withdrawn_at);

    // Claimed input conflicts instead of racing the claim.
    let error = withdraw_input(&db, "agent-a", "m-claimed", Utc::now())?.unwrap_err();
    assert!(matches!(
        error,
        InputWithdrawalError::AlreadyClaimed { .. }
    ));

    // Non-operator input is out of scope.
    let error = withdraw_input(&db, "agent-a", "m-system", Utc::now())?.unwrap_err();
    assert_eq!(error, InputWithdrawalError::NotOperatorInput);

    // Unknown message id is a typed not-found.
    let error = withdraw_input(&db, "agent-a", "m-missing", Utc::now())?.unwrap_err();
    assert_eq!(error, InputWithdrawalError::NotFound);
    let _ = Priority::Normal;
    Ok(())
}
