//! Compatibility for exact, unpublished feature lineages before integration.
use super::migrations::MIGRATIONS;
use anyhow::{ensure, Result};
use rusqlite::Connection;

pub(super) fn reconcile_feature_history(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    let records = {
        let mut query = transaction.prepare(
            "SELECT version,name FROM schema_migrations WHERE version>=81 ORDER BY version",
        )?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let legacy_start = records
        .iter()
        .find(|(version, name)| {
            matches!(*version, 81 | 83 | 89) && name == "episode_memory_results"
        })
        .map(|(version, _)| *version);
    let Some(legacy_start) = legacy_start else {
        // Remapped features can precede missing published migrations. Each
        // segment must still be an exact prefix, including provider v89/v90.
        let published_len = records
            .iter()
            .take_while(|(version, _)| *version < 91)
            .count();
        let (published, features) = records.split_at(published_len);
        ensure!(
            [(published, 81), (features, 91)]
                .into_iter()
                .all(|(prefix, start)| {
                    prefix.iter().enumerate().all(|(index, (version, name))| {
                        *version == start + index as i64
                            && MIGRATIONS.iter().any(|migration| {
                                migration.version == *version && migration.name == name
                            })
                    })
                }),
            "unrecognized migration history; refusing to modify the ledger"
        );
        return Ok(());
    };
    let prefix = records
        .iter()
        .take_while(|(version, _)| *version < legacy_start);
    ensure!(
        prefix.clone().enumerate().all(|(index, (version, name))| {
            *version == 81 + index as i64
                && MIGRATIONS
                    .iter()
                    .any(|migration| migration.version == *version && migration.name == name)
        }) && prefix.count() == (legacy_start - 81) as usize,
        "unrecognized published prefix before feature migration history"
    );
    let records = records
        .iter()
        .filter(|(version, _)| *version >= legacy_start)
        .collect::<Vec<_>>();
    let offset = 91 - legacy_start;
    let expected: Vec<_> = MIGRATIONS
        .iter()
        .filter(|migration| (91..=102).contains(&migration.version))
        .collect();
    ensure!(
        records.len() <= expected.len()
            && records
                .iter()
                .zip(&expected)
                .all(|((version, name), migration)| {
                    *version == migration.version - offset && name == migration.name
                }),
        "unrecognized unpublished feature migration history; refusing to remap"
    );
    // Descending updates avoid occupied destination keys. Preserve names and
    // timestamps; fixed-domain captures must never run a second time.
    for (version, name) in records.iter().rev() {
        ensure!(
            transaction.execute(
                "UPDATE schema_migrations SET version=version+?3 WHERE version=?1 AND name=?2",
                rusqlite::params![version, name, offset],
            )? == 1,
            "feature migration history changed during remap"
        );
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "migration_history_tests.rs"]
mod tests;
