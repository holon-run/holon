use anyhow::Result;
use rusqlite::{params, Connection};

use super::{namespace_ids_sql, sql_text, TaskFamily};
use crate::runtime_db::{
    connection::{
        configure_new_database_auto_vacuum, configure_persistent_database, open_connection,
    },
    migrations::{apply_migration, current_schema_version, ensure_migration_table, MIGRATIONS},
    RuntimeDb,
};

fn read_reverse_ids(connection: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut query = connection.prepare(sql)?;
    let ids = query
        .query_map(
            params![
                "hot",
                1,
                "result_settlement",
                None::<String>,
                -1,
                None::<String>
            ],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

#[test]
fn task_result_reverse_index_upgrades_existing_database_and_bounds_both_queries() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("upgrade.sqlite");
    let mut connection = open_connection(&path)?;
    configure_new_database_auto_vacuum(&connection)?;
    configure_persistent_database(&connection)?;
    ensure_migration_table(&connection)?;
    let index_migration = MIGRATIONS
        .iter()
        .find(|migration| migration.name == "index_task_result_settlement_by_task")
        .expect("TaskResult reverse index migration");
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version < index_migration.version)
    {
        apply_migration(&mut connection, migration)?;
    }
    let tx = connection.transaction()?;
    for (id, agent, task, availability, time) in [
        ("r-b", "hot", "task's result", Some("present"), "2026-01-01"),
        ("r-a", "hot", "task's result", Some("present"), "2026-02-01"),
        ("r-c", "hot", "task's result", Some("pruned"), "2026-03-01"),
        ("r-d", "hot", "task's result", None, "2026-04-01"),
        ("r-e", "hot", "different", Some("present"), "2026-05-01"),
        (
            "r-f",
            "other",
            "task's result",
            Some("present"),
            "2026-06-01",
        ),
    ] {
        tx.execute(
            "INSERT INTO task_result_settlements
             (result_identity,agent_id,task_id,message_id,rejoin_generation,parent_turn_id,state,
              disposition,created_at,updated_at,settled_at,payload_json)
             VALUES (?1,?2,?3,?1,1,'turn','settled','model_delivered',?4,?4,?4,'{}')",
            params![id, agent, task, time],
        )?;
        if let Some(availability) = availability {
            let digest = (availability == "present").then_some("digest");
            tx.execute(
                "INSERT INTO task_domain_records
                 (agent_id,identity_incarnation,family,record_id,revision,availability,content_digest,projection_digest)
                 VALUES (?1,1,'result_settlement',?2,1,?3,?4,?4)",
                params![agent, id, availability, digest],
            )?;
        }
    }
    tx.commit()?;
    let predicate = format!("c.task_id={}", sql_text("task's result"));
    let sql = namespace_ids_sql(TaskFamily::ResultSettlement, &predicate, None);
    assert_eq!(
        current_schema_version(&connection)?,
        index_migration.version - 1
    );
    let before = read_reverse_ids(&connection, &sql)?;
    assert_eq!(before, ["r-a", "r-b"]);
    drop(connection);

    let db = RuntimeDb::open_and_migrate(&path, path.with_extension("lock"))?;
    let connection = db.connection()?;
    assert!(current_schema_version(&connection)? >= index_migration.version);
    assert_eq!(read_reverse_ids(&connection, &sql)?, before);
    let count_sql = format!(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(result_identity AS BLOB))+128),0) FROM ({sql})"
    );
    for query in [&sql, &count_sql] {
        let plan = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))?
            .query_map(
                params![
                    "hot",
                    1,
                    "result_settlement",
                    None::<String>,
                    -1,
                    None::<String>
                ],
                |row| row.get::<_, String>(3),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(plan.iter().any(|step| step.contains("USING COVERING INDEX idx_task_result_settlements_agent_task_identity (agent_id=? AND task_id=?)")), "{plan:?}");
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
    Ok(())
}
