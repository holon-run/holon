use std::sync::{Arc, Barrier};

use anyhow::Result;
use rusqlite::params;

use super::*;
use crate::types::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility,
    WorkItemRecord, WorkItemState,
};

#[test]
fn task_and_timer_incarnation_and_data_share_snapshot_during_replacement_and_deletion() -> Result<()>
{
    for status in ["active", "deleting"] {
        let home = tempfile::tempdir()?;
        let db_path = home.path().join("runtime.sqlite");
        let lock_path = home.path().join("runtime.lock");
        let db = RuntimeDb::open_and_migrate(&db_path, &lock_path)?;
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            "list-agent",
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let now = chrono::Utc::now();
        let task: TaskRecord = serde_json::from_value(serde_json::json!({
            "id": "task-list", "agent_id": "list-agent", "kind": "command_task",
            "status": "completed", "created_at": now, "updated_at": now, "summary": "original"
        }))?;
        let timer: TimerRecord = serde_json::from_value(serde_json::json!({
            "id": "timer-list", "agent_id": "list-agent", "created_at": now,
            "duration_ms": 1000, "repeat": false, "summary": "original"
        }))?;
        db.tasks().upsert(&task)?;
        db.timers().upsert(&timer)?;
        let barrier = Arc::new(Barrier::new(2));
        let writer_barrier = Arc::clone(&barrier);
        let writer = std::thread::spawn(move || -> Result<()> {
            let writer_db = RuntimeDb::open_and_migrate(db_path, lock_path)?;
            writer_barrier.wait();
            let result = writer_db.transaction(|tx| {
                tx.execute(
                    "UPDATE agent_identities SET status = ?1,
                     payload_json = json_set(payload_json, '$.incarnation', 2, '$.status', ?1)
                     WHERE agent_id = 'list-agent'",
                    [status],
                )?;
                for table in ["tasks", "timers"] {
                    tx.execute(
                        &format!(
                            "UPDATE {table} SET payload_json =
                             json_set(payload_json, '$.summary', 'replacement-only')"
                        ),
                        [],
                    )?;
                }
                Ok(())
            });
            writer_barrier.wait();
            result
        });
        let during = db.with_read(|connection| {
            read_agent_record_snapshot(connection, "list-agent", Some(1), |view| {
                barrier.wait();
                barrier.wait();
                Ok((
                    query_tasks_for_agent(view, "owner_agent_id = ?1", ["list-agent"], 20)?,
                    recent_timers_for_agent(view, "list-agent", 50)?,
                ))
            })
        })?;
        writer.join().expect("replacement writer")?;
        let (identity, (tasks, timers)) = during.expect("original snapshot");
        assert_eq!(identity.agent_incarnation, 1);
        assert_eq!(tasks, vec![task]);
        assert_eq!(timers, vec![timer]);
        assert!(matches!(
            db.agent_tasks_snapshot("list-agent", Some(1), true, 20)
                .unwrap_err()
                .downcast_ref::<ConversationReadError>(),
            Some(ConversationReadError::IdentityChanged)
        ));
        assert!(matches!(
            db.agent_timers_snapshot("list-agent", Some(1), 50)
                .unwrap_err()
                .downcast_ref::<ConversationReadError>(),
            Some(ConversationReadError::IdentityChanged)
        ));
        let tasks = db.agent_tasks_snapshot("list-agent", None, true, 20)?;
        let timers = db.agent_timers_snapshot("list-agent", None, 50)?;
        if status == "active" {
            let (identity, tasks) = tasks.expect("replacement task snapshot");
            assert_eq!(identity.agent_incarnation, 2);
            assert_eq!(tasks[0].summary.as_deref(), Some("replacement-only"));
            let (identity, timers) = timers.expect("replacement timer snapshot");
            assert_eq!(identity.agent_incarnation, 2);
            assert_eq!(timers[0].summary.as_deref(), Some("replacement-only"));
            for table in ["tasks", "timers"] {
                db.connection()?.execute(
                    &format!("UPDATE {table} SET payload_json = 'invalid replacement payload'"),
                    [],
                )?;
            }
            assert!(matches!(
                db.agent_tasks_snapshot("list-agent", Some(1), true, 20)
                    .unwrap_err()
                    .downcast_ref::<ConversationReadError>(),
                Some(ConversationReadError::IdentityChanged)
            ));
            assert!(matches!(
                db.agent_timers_snapshot("list-agent", Some(1), 50)
                    .unwrap_err()
                    .downcast_ref::<ConversationReadError>(),
                Some(ConversationReadError::IdentityChanged)
            ));
        } else {
            assert!(tasks.is_none());
            assert!(timers.is_none());
        }
    }
    Ok(())
}

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
