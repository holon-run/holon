//! Tests for the deferred-report probe behind
//! `canonical_input_binding_matches_tx`.

use super::{deferred_report_pending_tx, DEFERRED_REPORT_PENDING_SQL};
use crate::{
    runtime_db::{evidence::insert_tool_evidence_tx, RuntimeDb},
    tool::names::{COMPLETE_WORK_ITEM, SLEEP, WAIT_FOR},
    types::{AuthorityClass, ToolExecutionRecord, ToolExecutionStatus},
};

type Result<T> = std::result::Result<T, anyhow::Error>;

fn database() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let directory = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        directory.path().join("runtime.sqlite"),
        directory.path().join("runtime.lock"),
    )?;
    Ok((directory, db))
}

fn tool_record(
    id: &str,
    agent_id: &str,
    turn_id: Option<&str>,
    tool_name: &str,
    status: ToolExecutionStatus,
) -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: id.into(),
        agent_id: agent_id.into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: turn_id.map(Into::into),
        tool_name: tool_name.into(),
        created_at: chrono::Utc::now(),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status,
        input: serde_json::json!({}),
        output: serde_json::json!({}),
        summary: "deferred-report probe fixture".into(),
        call_binding: None,
        invocation_surface: None,
    }
}

fn pending(db: &RuntimeDb, agent_id: &str, turn_id: &str) -> Result<bool> {
    db.transaction(|tx| deferred_report_pending_tx(tx, agent_id, turn_id))
}

fn query_plan(db: &RuntimeDb, sql: &str) -> Result<Vec<String>> {
    let connection = db.connection()?;
    let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    let details = statement
        .query_map(rusqlite::params!["agent-a", "turn-target"], |row| {
            row.get::<_, String>(3)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(details)
}

#[test]
fn deferred_wait_for_and_complete_work_item_flag_target_turn() -> Result<()> {
    let (_directory, db) = database()?;
    db.evidence().append_tool_execution(&tool_record(
        "tool-wait",
        "agent-a",
        Some("turn-target"),
        WAIT_FOR,
        ToolExecutionStatus::Deferred,
    ))?;
    assert!(pending(&db, "agent-a", "turn-target")?);
    assert!(!pending(&db, "agent-a", "turn-clean")?);

    db.evidence().append_tool_execution(&tool_record(
        "tool-complete",
        "agent-a",
        Some("turn-complete"),
        COMPLETE_WORK_ITEM,
        ToolExecutionStatus::Deferred,
    ))?;
    assert!(pending(&db, "agent-a", "turn-complete")?);
    Ok(())
}

#[test]
fn foreign_tools_statuses_agents_and_turns_do_not_flag() -> Result<()> {
    let (_directory, db) = database()?;
    let rows = [
        ("other-tool", "agent-a", Some("turn-target"), SLEEP),
        ("success-wait", "agent-a", Some("turn-target"), WAIT_FOR),
        (
            "interrupted-complete",
            "agent-a",
            Some("turn-target"),
            COMPLETE_WORK_ITEM,
        ),
        ("foreign-agent", "agent-b", Some("turn-target"), WAIT_FOR),
        ("foreign-turn", "agent-a", Some("turn-other"), WAIT_FOR),
        ("legacy-null-turn", "agent-a", None, WAIT_FOR),
    ];
    let statuses = [
        ToolExecutionStatus::Deferred,
        ToolExecutionStatus::Success,
        ToolExecutionStatus::Interrupted,
        ToolExecutionStatus::Deferred,
        ToolExecutionStatus::Deferred,
        ToolExecutionStatus::Deferred,
    ];
    for ((id, agent, turn, tool), status) in rows.iter().zip(statuses) {
        db.evidence()
            .append_tool_execution(&tool_record(id, agent, *turn, tool, status))?;
    }
    assert!(!pending(&db, "agent-a", "turn-target")?);
    // The foreign turn's deferred record still fences its own turn.
    assert!(pending(&db, "agent-a", "turn-other")?);
    // Legacy rows without a turn fence no turn under either reading.
    assert!(!pending(&db, "agent-a", "turn-legacy")?);
    Ok(())
}

#[test]
fn appended_rows_keep_column_and_payload_turn_id_agreed() -> Result<()> {
    let (_directory, db) = database()?;
    db.evidence().append_tool_execution(&tool_record(
        "with-turn",
        "agent-a",
        Some("turn-x"),
        WAIT_FOR,
        ToolExecutionStatus::Deferred,
    ))?;
    db.evidence().append_tool_execution(&tool_record(
        "without-turn",
        "agent-a",
        None,
        SLEEP,
        ToolExecutionStatus::Success,
    ))?;
    let connection = db.connection()?;
    let mut statement = connection
        .prepare("SELECT turn_id, json_extract(payload_json, '$.turn_id') FROM tool_executions")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 2);
    for (column, payload) in rows {
        assert_eq!(column, payload);
    }
    Ok(())
}

#[test]
fn probe_plan_is_scoped_and_stays_accurate_with_foreign_history() -> Result<()> {
    let (_directory, db) = database()?;
    db.transaction(|tx| {
        for index in 0..5 {
            insert_tool_evidence_tx(
                tx,
                &tool_record(
                    &format!("tool-target-{index}"),
                    "agent-a",
                    Some("turn-target"),
                    SLEEP,
                    ToolExecutionStatus::Success,
                ),
            )?;
        }
        for index in 0..5_000 {
            insert_tool_evidence_tx(
                tx,
                &tool_record(
                    &format!("tool-history-{index}"),
                    "agent-a",
                    Some(&format!("turn-history-{index}")),
                    SLEEP,
                    ToolExecutionStatus::Success,
                ),
            )?;
        }
        Ok(())
    })?;
    assert!(!pending(&db, "agent-a", "turn-target")?);

    db.evidence().append_tool_execution(&tool_record(
        "tool-late-deferred",
        "agent-a",
        Some("turn-target"),
        WAIT_FOR,
        ToolExecutionStatus::Deferred,
    ))?;
    assert!(pending(&db, "agent-a", "turn-target")?);

    let scoped = query_plan(&db, DEFERRED_REPORT_PENDING_SQL)?;
    assert!(
        scoped
            .iter()
            .any(|line| line.contains("idx_tool_executions_agent_turn")
                && line.contains("agent_id=?")
                && line.contains("turn_id=?")),
        "scoped plan missing agent+turn index range: {scoped:?}"
    );
    let legacy_sql =
        DEFERRED_REPORT_PENDING_SQL.replace("agent_id = ?1 AND turn_id = ?2", "agent_id = ?1");
    let legacy = query_plan(&db, &legacy_sql)?;
    assert!(
        legacy
            .iter()
            .any(|line| line.contains("agent_id=?") && !line.contains("turn_id=?")),
        "legacy plan should use only the agent prefix: {legacy:?}"
    );
    Ok(())
}
