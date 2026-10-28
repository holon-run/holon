use super::*;
use crate::runtime_db::migrations::ensure_migration_table;

#[path = "migration_data_tests.rs"]
mod data;

fn legacy_prefix(start: i64, count: usize) -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    ensure_migration_table(&connection)?;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| (81..start).contains(&migration.version))
    {
        connection.execute(
            "INSERT INTO schema_migrations VALUES (?1,?2,'published-timestamp')",
            rusqlite::params![migration.version, migration.name],
        )?;
    }
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| (91..=102).contains(&migration.version))
        .take(count)
    {
        connection.execute(
            "INSERT INTO schema_migrations VALUES (?1,?2,'original-timestamp')",
            rusqlite::params![migration.version - (91 - start), migration.name],
        )?;
    }
    Ok(connection)
}

fn history(connection: &Connection) -> Result<Vec<(i64, String, String)>> {
    let mut query = connection
        .prepare("SELECT version,name,applied_at FROM schema_migrations ORDER BY version")?;
    let rows = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[test]
fn feature_history_every_prefix_preserves_names_timestamps_and_is_idempotent() -> Result<()> {
    for start in [81, 83, 89] {
        for count in 0..=12 {
            let mut connection = legacy_prefix(start, count)?;
            let old = history(&connection)?;
            reconcile_feature_history(&mut connection)?;
            let expected: Vec<_> = old
                .into_iter()
                .map(|(version, name, at)| {
                    let version = if version >= start {
                        version + (91 - start)
                    } else {
                        version
                    };
                    (version, name, at)
                })
                .collect();
            assert_eq!(history(&connection)?, expected);
            reconcile_feature_history(&mut connection)?;
            assert_eq!(history(&connection)?, expected);
        }
    }
    Ok(())
}

#[test]
fn feature_history_refuses_mixed_gapped_and_unknown_versions_without_changes() -> Result<()> {
    for start in [81, 83, 89] {
        for damage in [
            format!("UPDATE schema_migrations SET name='agent_template_remote_source_retry_deadline' WHERE version={}", start + 1),
            format!("DELETE FROM schema_migrations WHERE version={}", start + 1),
            format!("DELETE FROM schema_migrations WHERE version={start}"),
            format!("DELETE FROM schema_migrations WHERE version<={}", start + 1),
            format!("INSERT INTO schema_migrations VALUES ({},'unknown','unrecognized')", start + 12),
            format!("UPDATE schema_migrations SET name='provider_candidate_payment_required' WHERE version={start}"),
            "UPDATE schema_migrations SET name='unknown' WHERE version=81".to_owned(),
            "DELETE FROM schema_migrations WHERE version=81".to_owned(),
        ] {
            let mut connection = legacy_prefix(start, 12)?;
            connection.execute_batch(&damage)?;
            let before = history(&connection)?;
            assert!(reconcile_feature_history(&mut connection).is_err());
            assert_eq!(history(&connection)?, before);
        }
    }
    Ok(())
}

#[test]
fn feature_history_sql_failure_rolls_back_the_whole_mapping() -> Result<()> {
    for start in [81, 83, 89] {
        let mut connection = legacy_prefix(start, 12)?;
        connection.execute_batch(
            "CREATE TRIGGER reject_history BEFORE UPDATE ON schema_migrations
             WHEN OLD.version=89 BEGIN SELECT RAISE(ABORT,'injected history failure'); END;",
        )?;
        let before = history(&connection)?;
        assert!(reconcile_feature_history(&mut connection).is_err());
        assert_eq!(history(&connection)?, before);
    }
    Ok(())
}

#[test]
fn feature_history_leaves_fresh_and_published_upstream_history_unchanged() -> Result<()> {
    let mut connection = legacy_prefix(81, 0)?;
    reconcile_feature_history(&mut connection)?;
    assert!(history(&connection)?.is_empty());
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| (81..=90).contains(&migration.version))
    {
        connection.execute(
            "INSERT INTO schema_migrations VALUES (?1,?2,'published-timestamp')",
            rusqlite::params![migration.version, migration.name],
        )?;
        let before = history(&connection)?;
        reconcile_feature_history(&mut connection)?;
        assert_eq!(history(&connection)?, before);
    }
    Ok(())
}

#[test]
fn feature_history_leaves_current_prefixes_and_published_provider_migrations_unchanged(
) -> Result<()> {
    for count in 0..=12 {
        let mut connection = legacy_prefix(91, count)?;
        let before = history(&connection)?;
        reconcile_feature_history(&mut connection)?;
        assert_eq!(history(&connection)?, before);
    }
    Ok(())
}

#[test]
fn feature_history_refuses_gapped_mixed_and_unknown_current_history_without_changes() -> Result<()>
{
    for damage in [
        "DELETE FROM schema_migrations WHERE version=85",
        "DELETE FROM schema_migrations WHERE version=91",
        "DELETE FROM schema_migrations WHERE version=96",
        "UPDATE schema_migrations SET name='episode_memory_results' WHERE version=90",
        "UPDATE schema_migrations SET name='unknown' WHERE version=98",
        "INSERT INTO schema_migrations VALUES (103,'original_current_read_qualification','unknown')",
    ] {
        let mut connection = legacy_prefix(91, 12)?;
        connection.execute_batch(damage)?;
        let before = history(&connection)?;
        assert!(reconcile_feature_history(&mut connection).is_err());
        assert_eq!(history(&connection)?, before);
    }
    Ok(())
}

#[test]
fn feature_history_remapped_prefixes_accept_published_catchup_without_changes() -> Result<()> {
    for start in [81, 83, 89] {
        for count in 0..=12 {
            let mut connection = legacy_prefix(start, count)?;
            reconcile_feature_history(&mut connection)?;
            for migration in MIGRATIONS
                .iter()
                .filter(|migration| (81..=90).contains(&migration.version))
            {
                connection.execute(
                    "INSERT OR IGNORE INTO schema_migrations VALUES (?1,?2,'published-timestamp')",
                    rusqlite::params![migration.version, migration.name],
                )?;
                let before = history(&connection)?;
                reconcile_feature_history(&mut connection)?;
                assert_eq!(history(&connection)?, before);
            }
        }
    }
    Ok(())
}

#[test]
fn published_and_feature_lineage_versions_and_names_remain_exact() {
    let expected = [
        (81, "messages_trusted_operator_anchor_index"),
        (82, "agent_template_remote_source_retry_deadline"),
        (83, "task_result_recovery_budget"),
        (84, "interrupted_replay_source_final_settlement"),
        (85, "briefs_agent_latest_index"),
        (86, "provider_admission_fences"),
        (87, "agent_message_wait_reply_outcomes"),
        (88, "provider_route_throttle_and_rate_limit_outcomes"),
        (89, "provider_candidate_payment_required"),
        (90, "provider_cooldown_safe_cause"),
        (91, "episode_memory_results"),
        (92, "explicit_episode_continuity"),
        (93, "control_command_receipts_and_cancellation_obligations"),
        (94, "qualified_input_evidence"),
        (95, "original_input_mutation_receipts"),
        (96, "qualified_work_wait_continuation"),
        (97, "actual_dispatch_memory_responsibility"),
        (98, "qualified_task_result_domain"),
        (99, "qualified_scheduling_domain"),
        (100, "original_input_birth_frontier"),
        (101, "original_current_read_qualification"),
        (102, "original_turnless_command_creation"),
    ];
    let actual: Vec<_> = MIGRATIONS
        .iter()
        .filter(|migration| (81..=102).contains(&migration.version))
        .map(|migration| (migration.version, migration.name))
        .collect();
    assert_eq!(actual.as_slice(), expected.as_slice());
}
