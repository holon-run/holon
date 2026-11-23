use super::*;

#[test]
fn retire_destructive_operation_scheduler_accepts_compacted_baselines() -> Result<()> {
    let migration = MIGRATIONS
        .iter()
        .find(|migration| migration.name == "retire_destructive_operation_scheduler")
        .unwrap();
    for has_metadata in [false, true] {
        let mut db = Connection::open_in_memory()?;
        ensure_migration_table(&db)?;
        db.execute_batch(
            "INSERT INTO schema_migrations VALUES (120, 'turn_content_deletion', 'fixture');
             CREATE TABLE destructive_operations (operation_id TEXT PRIMARY KEY);",
        )?;
        if has_metadata {
            db.execute_batch(
                "CREATE TABLE runtime_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO runtime_metadata VALUES
                    ('destructive_operation_closed:old', 'fixture'),
                    ('unrelated', 'preserved');",
            )?;
        }

        for _ in 0..2 {
            apply_migration(&mut db, migration, unexpected_runtime_backfill)?;
            assert_eq!(current_schema_version(&db)?, migration.version);
            let tx = db.transaction()?;
            assert!(!table_exists_tx(&tx, "turn_records")?);
            assert!(!table_exists_tx(&tx, "destructive_operations")?);
            assert_eq!(table_exists_tx(&tx, "runtime_metadata")?, has_metadata);
            if has_metadata {
                let keys = tx
                    .prepare("SELECT key FROM runtime_metadata")?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(keys, ["unrelated"]);
            }
        }
    }
    Ok(())
}
