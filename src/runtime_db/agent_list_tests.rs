use std::sync::{Arc, Barrier};

use anyhow::Result;
use rusqlite::params;

use super::*;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
    WorkItemRecord, WorkItemState,
};

#[test]
fn work_queue_incarnation_and_data_share_snapshot_during_replacement_and_deletion() -> Result<()> {
    for status in ["active", "deleting"] {
        let home = tempfile::tempdir()?;
        let db_path = home.path().join("runtime.sqlite");
        let lock_path = home.path().join("runtime.lock");
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        let identity = AgentIdentityRecord::new(
            "queue-agent",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        db.agent_identities().upsert(&identity)?;
        let item = WorkItemRecord::new("queue-agent", "original", WorkItemState::Open);
        db.work_items().insert_new(&item)?;
        let barrier = Arc::new(Barrier::new(2));
        let writer_barrier = Arc::clone(&barrier);
        let writer = std::thread::spawn(move || -> Result<()> {
            let writer_db = RuntimeDb::open_and_migrate(db_path, lock_path)?;
            writer_barrier.wait();
            let result = writer_db.transaction(|tx| {
                tx.execute(
                    "UPDATE agent_identities SET status = ?1,
                     payload_json = json_set(payload_json, '$.incarnation', 2, '$.status', ?1)
                     WHERE agent_id = 'queue-agent'",
                    [status],
                )?;
                tx.execute(
                    "UPDATE work_items SET objective = 'replacement-only',
                     payload_json = json_set(payload_json, '$.objective', 'replacement-only')
                     WHERE agent_id = 'queue-agent'",
                    [],
                )?;
                Ok(())
            });
            writer_barrier.wait();
            result
        });
        let during = db.with_read(|connection| {
            read_work_queue_snapshot(connection, "queue-agent", Some(1), || {
                barrier.wait();
                barrier.wait();
            })
        })?;
        writer.join().expect("replacement writer")?;
        let (identity, row) = during.expect("admitted original snapshot");
        assert_eq!(identity.agent_id, "queue-agent");
        assert_eq!(identity.agent_incarnation, 1);
        assert_eq!(row.facts.unwrap().open_work_items, vec![item]);
        let error = db
            .agent_work_queue_snapshot("queue-agent", Some(1))
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ConversationReadError>(),
            Some(ConversationReadError::IdentityChanged)
        ));
        let after = db.agent_work_queue_snapshot("queue-agent", None)?;
        if status == "active" {
            let (identity, row) = after.expect("replacement snapshot");
            assert_eq!(identity.agent_incarnation, 2);
            assert_eq!(
                row.facts.unwrap().open_work_items[0].objective,
                "replacement-only"
            );
            // A stale identity must fail before decoding replacement data.
            db.connection()?.execute(
                "UPDATE work_items SET payload_json = ?1 WHERE agent_id = 'queue-agent'",
                params!["invalid replacement payload"],
            )?;
            assert!(matches!(
                db.agent_work_queue_snapshot("queue-agent", Some(1))
                    .unwrap_err()
                    .downcast_ref::<ConversationReadError>(),
                Some(ConversationReadError::IdentityChanged)
            ));
        } else {
            assert!(after.is_none());
        }
    }
    Ok(())
}
