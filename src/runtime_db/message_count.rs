//! Exact counts of canonical Message rows, maintained by their SQLite transaction.

use anyhow::Result;
use rusqlite::Connection;

pub(super) const SCHEMA: &str = r#"
CREATE TABLE message_counts (
    agent_id TEXT PRIMARY KEY NOT NULL,
    message_count INTEGER NOT NULL CHECK(typeof(message_count) = 'integer' AND message_count >= 0)
) WITHOUT ROWID;

INSERT INTO message_counts (agent_id, message_count)
SELECT agent_id, COUNT(*) FROM messages GROUP BY agent_id;

CREATE TRIGGER message_counts_insert AFTER INSERT ON messages
BEGIN
    INSERT INTO message_counts (agent_id, message_count) VALUES (NEW.agent_id, 1)
    ON CONFLICT(agent_id) DO UPDATE SET message_count = message_count + 1;
END;

CREATE TRIGGER message_counts_delete AFTER DELETE ON messages
BEGIN
    UPDATE message_counts SET message_count = message_count - 1 WHERE agent_id = OLD.agent_id;
    DELETE FROM message_counts WHERE agent_id = OLD.agent_id AND message_count = 0;
END;

CREATE TRIGGER message_counts_move AFTER UPDATE OF agent_id ON messages
WHEN OLD.agent_id <> NEW.agent_id
BEGIN
    UPDATE message_counts SET message_count = message_count - 1 WHERE agent_id = OLD.agent_id;
    DELETE FROM message_counts WHERE agent_id = OLD.agent_id AND message_count = 0;
    INSERT INTO message_counts (agent_id, message_count) VALUES (NEW.agent_id, 1)
    ON CONFLICT(agent_id) DO UPDATE SET message_count = message_count + 1;
END;
"#;

pub(super) fn for_agent(connection: &Connection, agent_id: &str) -> Result<usize> {
    Ok(connection.query_row(
        "SELECT COALESCE((SELECT message_count FROM message_counts WHERE agent_id=?1), 0)",
        [agent_id],
        |row| row.get(0),
    )?)
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
mod tests {
    use super::*;

    #[test]
    fn upgrade_counts_existing_rows_and_tracks_actual_transaction_changes() -> Result<()> {
        let mut connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE messages (evidence_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL);
             INSERT INTO messages VALUES ('old-a', 'a'), ('old-b', 'b');",
        )?;
        let migration = crate::runtime_db::migrations::MIGRATIONS
            .iter()
            .find(|migration| migration.name == "message_row_counts")
            .expect("Message count migration");
        crate::runtime_db::migrations::ensure_migration_table(&connection)?;
        crate::runtime_db::migrations::apply_migration(&mut connection, migration)?;

        let assert_counts = |db: &Connection, expected: (usize, usize)| -> Result<()> {
            assert_eq!((for_agent(db, "a")?, for_agent(db, "b")?), expected);
            for agent in ["a", "b"] {
                let actual: usize = db.query_row(
                    "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
                    [agent],
                    |row| row.get(0),
                )?;
                assert_eq!(for_agent(db, agent)?, actual);
            }
            Ok(())
        };
        assert_counts(&connection, (1, 1))?;
        let tx = connection.transaction()?;
        tx.execute_batch(
            "INSERT INTO messages VALUES ('new-a', 'a');
             INSERT INTO messages VALUES ('new-a', 'a')
             ON CONFLICT(evidence_id) DO UPDATE SET agent_id=excluded.agent_id;
             DELETE FROM messages WHERE evidence_id = 'old-b';",
        )?;
        assert_counts(&tx, (2, 0))?;
        tx.rollback()?;
        assert_counts(&connection, (1, 1))?;

        let tx = connection.transaction()?;
        tx.execute_batch(
            "INSERT INTO messages VALUES ('new-a', 'a');
             UPDATE messages SET agent_id='b' WHERE evidence_id='new-a';
             DELETE FROM messages WHERE agent_id='a';",
        )?;
        tx.commit()?;
        assert_counts(&connection, (0, 2))?;
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM message_counts WHERE agent_id='a'",
                [],
                |row| row.get::<_, usize>(0),
            )?,
            0
        );
        Ok(())
    }
}
