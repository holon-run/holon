//! Regression coverage for the #4445 startup seal backfill.
//!
//! A clean shutdown clears `agent_states.current_run_id` before a cross-version
//! upgrade, so the fixed v99 upgrade predicate seals nothing for a dispatch that
//! recovery still resolves (`MissingStart`). The backfill must restore exactly
//! that address from the migrated database and refuse every unprovable case.

use super::*;
use crate::runtime_db::migrations::{apply_migration, backfill_legacy_dispatch_seals, MIGRATIONS};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn stopped_upgrade_fixture() -> Result<Fixture> {
    let f = Fixture::new(false)?;
    f.db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_states SET current_run_id=NULL, status='asleep', payload_json=json_set(payload_json,'$.current_run_id',NULL,'$.status','asleep')",
            [],
        )?;
        tx.execute(
            "UPDATE turn_records SET terminal_kind='completed', completed_at=?1, payload_json=json_set(payload_json,'$.terminal',json_object('kind','completed'))",
            [f.at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)],
        )?;
        Ok(())
    })?;
    Ok(f)
}

fn seal_rows(connection: &Connection) -> Result<Vec<(i64, String, String, u64)>> {
    let mut statement = connection.prepare(
        "SELECT identity_incarnation,turn_id,source_message_id,turn_index FROM dispatch_memory_legacy_dispatches ORDER BY turn_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn assert_not_repaired(mutate: impl FnOnce(&Fixture) -> Result<()>) -> Result<()> {
    let f = stopped_upgrade_fixture()?;
    mutate(&f)?;
    let mut connection = f.db.connection()?;
    backfill_legacy_dispatch_seals(&mut connection)?;
    assert!(seal_rows(&connection)?.is_empty());
    Ok(())
}

#[test]
fn clean_shutdown_upgrade_backfills_the_dispatch_v99_missed() -> Result<()> {
    let f = stopped_upgrade_fixture()?;
    let migration = MIGRATIONS
        .iter()
        .find(|m| m.name == "actual_dispatch_memory_responsibility")
        .unwrap();
    let mut connection = f.db.connection()?;
    connection.execute("DELETE FROM schema_migrations WHERE version=99", [])?;
    apply_migration(&mut connection, migration)?;
    assert!(
        seal_rows(&connection)?.is_empty(),
        "the fixed v99 predicate cannot seal a stopped dispatch"
    );
    backfill_legacy_dispatch_seals(&mut connection)?;
    assert_eq!(
        seal_rows(&connection)?,
        vec![(
            1,
            f.start.key.actual_dispatch_turn_id.clone(),
            f.source.id.clone(),
            1
        )]
    );
    backfill_legacy_dispatch_seals(&mut connection)?;
    assert_eq!(seal_rows(&connection)?.len(), 1, "backfill is idempotent");
    drop(connection);
    f.db.transaction(|tx| {
        assert!(is_legacy_dispatch_tx(
            tx,
            f.start.key(),
            f.start.invocation()
        )?);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn already_migrated_database_repairs_in_place_without_rerunning_v99() -> Result<()> {
    let f = stopped_upgrade_fixture()?;
    let mut connection = f.db.connection()?;
    assert!(seal_rows(&connection)?.is_empty());
    backfill_legacy_dispatch_seals(&mut connection)?;
    assert_eq!(seal_rows(&connection)?.len(), 1);
    backfill_legacy_dispatch_seals(&mut connection)?;
    assert_eq!(seal_rows(&connection)?.len(), 1);
    Ok(())
}

#[test]
fn backfill_skips_a_still_live_dispatch_run() -> Result<()> {
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute(
                "UPDATE agent_states SET current_run_id='run:live', payload_json=json_set(payload_json,'$.current_run_id','run:live')",
                [],
            )?;
            Ok(())
        })?;
        Ok(())
    })
}

#[test]
fn backfill_skips_unfinished_or_misindexed_turns() -> Result<()> {
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute(
                "UPDATE turn_records SET terminal_kind=NULL, completed_at=NULL, payload_json=json_remove(payload_json,'$.terminal')",
                [],
            )?;
            Ok(())
        })?;
        Ok(())
    })?;
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute(
                "UPDATE agent_states SET turn_index=2, payload_json=json_set(payload_json,'$.turn_index',2)",
                [],
            )?;
            Ok(())
        })?;
        Ok(())
    })
}

#[test]
fn backfill_skips_missing_or_reassigned_source_messages() -> Result<()> {
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute("DELETE FROM messages", [])?;
            Ok(())
        })?;
        Ok(())
    })?;
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute("UPDATE messages SET agent_id='other-agent'", [])?;
            Ok(())
        })?;
        Ok(())
    })?;
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute(
                "UPDATE turn_records SET payload_json=json_set(json_remove(payload_json,'$.trigger'),'$.input_message_ids',json('[]'))",
                [],
            )?;
            Ok(())
        })?;
        Ok(())
    })
}

#[test]
fn backfill_skips_state_without_current_binding_or_v99_record() -> Result<()> {
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute(
                "UPDATE agent_states SET payload_json=json_remove(payload_json,'$.current_execution_binding')",
                [],
            )?;
            Ok(())
        })?;
        Ok(())
    })?;
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute(
                "UPDATE turn_records SET created_at='2999-01-01T00:00:00.000Z'",
                [],
            )?;
            Ok(())
        })?;
        Ok(())
    })?;
    let f = stopped_upgrade_fixture()?;
    let mut connection = f.db.connection()?;
    connection.execute("DELETE FROM schema_migrations WHERE version=99", [])?;
    backfill_legacy_dispatch_seals(&mut connection)?;
    assert!(seal_rows(&connection)?.is_empty());
    Ok(())
}

#[test]
fn backfill_skips_archived_identity() -> Result<()> {
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute(
                "UPDATE agent_identities SET status='archived', payload_json=json_set(payload_json,'$.status','archived')",
                [],
            )?;
            Ok(())
        })?;
        Ok(())
    })
}

#[test]
fn backfill_never_overwrites_existing_responsibility_records() -> Result<()> {
    assert_not_repaired(|f| {
        f.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO dispatch_memory_checkpoints(agent_id,identity_incarnation,turn_id,source_message_id,turn_index,state,seed_digest,record_digest,payload_json) VALUES(?1,1,?2,?3,1,'started',?4,?4,'{}')",
                params![
                    &f.scope.agent_id,
                    &f.start.key.actual_dispatch_turn_id,
                    &f.source.id,
                    DIGEST
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    })?;
    let f = stopped_upgrade_fixture()?;
    let mut connection = f.db.connection()?;
    connection.execute(
        "INSERT INTO dispatch_memory_legacy_dispatches(agent_id,identity_incarnation,turn_id,source_message_id,turn_index) VALUES(?1,1,?2,'message:other',1)",
        params![&f.scope.agent_id, &f.start.key.actual_dispatch_turn_id],
    )?;
    backfill_legacy_dispatch_seals(&mut connection)?;
    assert_eq!(
        seal_rows(&connection)?,
        vec![(
            1,
            f.start.key.actual_dispatch_turn_id.clone(),
            "message:other".to_string(),
            1
        )]
    );
    Ok(())
}
