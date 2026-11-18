use anyhow::Result;
use holon_storage::{
    conversation::{
        CONVERSATION_ACTIVITY_BEFORE_SQL, CONVERSATION_HISTORY_BEFORE_SQL, HISTORY_AFTER_SQL,
    },
    migrations::{migrate, CONVERSATION_REPLAY_INPUT_SOURCE_SELECT_SQL},
};
use rusqlite::{params, Connection};

const AGENT_ID: &str = "agent-conversation-test";

fn migrated_connection() -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    // The query plans depend on the schema and indexes. The runtime backfills
    // populate business rows in existing databases.
    migrate(&mut connection, |_, _| Ok(()))?;
    Ok(connection)
}

#[test]
fn migration_replay_scan_query_plan_has_no_correlated_turn_scan() -> Result<()> {
    let connection = migrated_connection()?;
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
fn keyset_queries_use_declared_indexes_without_temp_sorting() -> Result<()> {
    let connection = migrated_connection()?;

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

    let forward_plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {HISTORY_AFTER_SQL}"))?
        .query_map(params![AGENT_ID, 5_i64, "m", 10_i64, "z", 10_i64], |row| {
            row.get::<_, String>(3)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(
        forward_plan
            .iter()
            .any(|detail| detail.contains("idx_turn_records_agent_keyset")),
        "forward history did not seek the native keyset index: {forward_plan:?}"
    );
    assert!(
        forward_plan
            .iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE")),
        "forward history used a temporary sort: {forward_plan:?}"
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
